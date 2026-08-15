//! Consistency checks for a plugin's parameter table, and a snapshot of it.
//!
//! A parameter's identity is spread across five places: the `PARAMS` table, the
//! `P_*` index constants, the preset tables, the GUI rows, and now the stepped
//! list. Nothing checked that they agreed — the stepped list was added as a
//! fifth parallel table during the review fixes, and the only thing keeping it
//! in sync with `PARAMS` is care.
//!
//! Two jobs here:
//!
//! **Consistency** — ids dense and ordered, defaults inside range, names unique
//! and non-empty, stepped params actually discrete. These are the mistakes a
//! hand-maintained table invites.
//!
//! **Stability** — the table's shape goes into the quality snapshot, so
//! changing a range, a default, or the order of parameters shows up as a diff
//! in review. Lesson 10 in the project's CLAUDE.md says reordering `PARAMS`
//! breaks every saved project that automates the plugin; until now the only
//! guard against that was memory.

use superduper_dsp_sdk::clap_helpers::ParamDef;

/// Panics with a specific message if the table is internally inconsistent.
pub fn check_table(plugin: &str, params: &[ParamDef], stepped: &[u32]) {
    let mut problems = Vec::new();

    for (i, p) in params.iter().enumerate() {
        let name = String::from_utf8_lossy(p.name).to_string();
        if p.id as usize != i {
            problems.push(format!(
                "  index {i} has id {} — ids must equal the index, or every lookup by id \
                 silently reads the wrong parameter",
                p.id
            ));
        }
        if name.trim().is_empty() {
            problems.push(format!("  index {i} has an empty name"));
        }
        if p.min > p.max {
            problems.push(format!("  {name}: min {} above max {}", p.min, p.max));
        }
        if p.default < p.min || p.default > p.max {
            problems.push(format!(
                "  {name}: default {} outside {}..={} — the host clamps it and the plugin \
                 starts up sounding different from its own table",
                p.default, p.min, p.max
            ));
        }
    }

    let mut names: Vec<_> = params
        .iter()
        .map(|p| String::from_utf8_lossy(p.name).to_string())
        .collect();
    names.sort();
    for pair in names.windows(2) {
        if pair[0] == pair[1] {
            problems.push(format!("  duplicate parameter name {:?}", pair[0]));
        }
    }

    for id in stepped {
        match params.iter().find(|p| p.id == *id) {
            None => problems.push(format!(
                "  stepped list names id {id}, which is not in PARAMS — a leftover from \
                 renumbering"
            )),
            Some(p) => {
                let span = p.max - p.min;
                if span > 40.0 {
                    problems.push(format!(
                        "  {} is declared stepped but spans {span} — hosts quantise stepped \
                         params, so a continuous one declared this way jumps",
                        String::from_utf8_lossy(p.name)
                    ));
                }
                if p.min.fract() != 0.0 || p.max.fract() != 0.0 {
                    problems.push(format!(
                        "  {} is declared stepped but its range is not whole numbers \
                         ({}..={})",
                        String::from_utf8_lossy(p.name),
                        p.min,
                        p.max
                    ));
                }
            }
        }
    }

    assert!(
        problems.is_empty(),
        "{plugin}'s parameter table is inconsistent:\n{}",
        problems.join("\n")
    );
}

/// Record the table's shape so a change to it shows up in review.
pub fn record_table(suite: &mut crate::Suite, params: &[ParamDef]) {
    suite.record("params_count", params.len() as f64);
    for p in params {
        let name = String::from_utf8_lossy(p.name).replace(' ', "_").to_lowercase();
        suite.record(format!("param_{:02}_{name}_min", p.id), p.min);
        suite.record(format!("param_{:02}_{name}_max", p.id), p.max);
        suite.record(format!("param_{:02}_{name}_default", p.id), p.default);
    }
}
