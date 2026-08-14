//! Enforce the "never inside `process()`" rules that CLAUDE.md writes down.
//!
//! Eight of the fifteen findings in the 2026-08-14 architecture review were
//! violations of rules this repo already documents: no heap allocation, no
//! mutex, no file I/O, no panicking. Documentation caught none of them — they
//! were introduced one plugin at a time and found months later by reading the
//! code. This test reads it instead, on every `cargo test`.
//!
//! It is a lexical scan, not a borrow-checker: it extracts each `fn process(`
//! body from every plugin crate and looks for the forbidden constructs. That
//! misses anything hidden behind a helper call, so it is a floor, not a
//! guarantee. It still would have caught the LinEQ FIR rebuild, the Looper's
//! per-block `vec!`, the NAM model clone and the Drum note logging.

use std::path::{Path, PathBuf};

/// (needle, why it is banned, allowed exceptions by crate name)
const BANNED: &[(&str, &str, &[&str])] = &[
    ("vec![", "heap allocation — pre-allocate in activate()", &[]),
    (".to_vec()", "heap allocation", &[]),
    ("Vec::new()", "heap allocation", &[]),
    ("Box::new(", "heap allocation", &[]),
    ("format!(", "heap allocation", &[]),
    (".lock()", "blocking mutex — use try_lock or atomics", &[]),
    ("slog!(", "file I/O + mutex; see sdk/src/log.rs", &[]),
    ("println!(", "syscall", &[]),
    (".unwrap()", "panic in the audio callback", &[]),
    (".expect(", "panic in the audio callback", &[]),
    ("panic!(", "panic in the audio callback", &[]),
    ("assert_eq!(", "panic in the audio callback", &[]),
];

fn effects_dir() -> PathBuf {
    // sdk/tests/ → repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("sdk has a parent")
        .join("effects")
}

/// Extract the body of every `fn process(` in `src`, by brace matching.
fn process_bodies(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut from = 0;
    while let Some(rel) = src[from..].find("fn process(") {
        let start = from + rel;
        // Find the opening brace of the body (skip the signature).
        let Some(brace_rel) = src[start..].find('{') else { break };
        let brace = start + brace_rel;
        let mut depth = 0usize;
        let mut end = brace;
        for (i, &b) in bytes.iter().enumerate().skip(brace) {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let line = src[..start].matches('\n').count() + 1;
        out.push((line, src[brace..=end].to_string()));
        from = end.max(start + 1);
    }
    out
}

/// Strip `//` comments so a rule quoted in prose doesn't trip the scan.
fn strip_comments(body: &str) -> String {
    body.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn process_bodies_obey_the_rt_rules() {
    let mut violations = Vec::new();
    let mut scanned = 0;

    for entry in std::fs::read_dir(effects_dir()).expect("effects/ exists") {
        let dir = entry.expect("readable entry").path();
        let lib = dir.join("src/lib.rs");
        if !lib.is_file() {
            continue;
        }
        let crate_name = dir.file_name().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(&lib).expect("readable lib.rs");
        for (line, body) in process_bodies(&src) {
            scanned += 1;
            let code = strip_comments(&body);
            for (needle, why, exempt) in BANNED {
                if exempt.contains(&crate_name.as_str()) {
                    continue;
                }
                if code.contains(needle) {
                    violations.push(format!(
                        "{crate_name}/src/lib.rs:{line} process() contains `{needle}` — {why}"
                    ));
                }
            }
        }
    }

    assert!(scanned > 20, "expected to scan every plugin, only saw {scanned}");
    assert!(
        violations.is_empty(),
        "RT-safety rules violated inside process():\n  {}\n\n\
         These are the rules in superduper-dsp/CLAUDE.md (\"DSP code style rules — \
         never violate inside process()\"). Move the work to activate() or to the \
         main thread via request_callback(), or hand it over with try_lock.",
        violations.join("\n  ")
    );
}
