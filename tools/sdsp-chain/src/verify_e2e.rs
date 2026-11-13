
//! End-to-end for `sdsp-chain verify`: the controls of tools/test_mixverify.py, run on the real binary.
//! The binary is the sibling of the test executable (`cargo build -p sdsp-chain` first); a missing binary
//! fails the test instead of skipping it. (An integration test target would not build here: cargo then
//! resolves the plugin crates against a second clack-plugin.)
//! bass + chord, an EQ cut on the chord at 300 Hz, and a bus log that says the clash fell 72 % -> 30 %.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn chain_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().unwrap().parent().unwrap().join("sdsp-chain");
    assert!(bin.is_file(), "build the binary first: cargo build --release -p sdsp-chain ({})", bin.display());
    bin
}

const SR: u32 = 48000;

fn tones(parts: &[(f64, f64)]) -> Vec<f64> {
    (0..(4 * SR) as usize).map(|i| parts.iter().map(|(hz, a)| a * (2.0 * std::f64::consts::PI * hz * i as f64 / SR as f64).sin()).sum()).collect()
}

fn write_wav(path: &Path, x: &[f64]) {
    let mut d = Vec::new();
    let data_len = (x.len() * 4) as u32;
    d.extend(b"RIFF");
    d.extend((36 + data_len).to_le_bytes());
    d.extend(b"WAVEfmt ");
    d.extend(16u32.to_le_bytes());
    d.extend(1u16.to_le_bytes());
    d.extend(2u16.to_le_bytes());
    d.extend(SR.to_le_bytes());
    d.extend((SR * 4).to_le_bytes());
    d.extend(4u16.to_le_bytes());
    d.extend(16u16.to_le_bytes());
    d.extend(b"data");
    d.extend(data_len.to_le_bytes());
    for v in x {
        let s = ((v * 32767.0) as i16).to_le_bytes();
        d.extend(s);
        d.extend(s);
    }
    std::fs::write(path, d).unwrap();
}

const BEFORE: &str = "[[track]]\nname = \"bass\"\ninput = \"bass.wav\"\n\n[[track]]\nname = \"chord\"\ninput = \"chord.wav\"\n\n[[master]]\nplugin = \"eq\"\n";
const CUT_300: &str = "  [[track.stage]]\n  plugin = \"eq\"\n  params = { \"Mid Freq\" = 300.0, \"Mid Gain\" = -6.0, \"Mid Q\" = 2.0 }\n";

fn run_line(label: &str, share: f64, chord_db: f64) -> String {
    let clashes = if share > 0.0 { format!(r#"[{{"a":"bass","b":"chord","low_hz":250.0,"high_hz":340.0,"share":{share},"louder":"bass","by_db":2.3}}]"#) } else { "[]".into() };
    format!(r#"{{"label":"{label}","seconds":4.0,"tracks":["bass","chord"],"clashes":{clashes},"buried":[],"levels":[{{"track":"Master","rms_db":-12.0}},{{"track":"bass","rms_db":-13.0}},{{"track":"chord","rms_db":{chord_db}}}]}}"#)
}

fn fixture() -> PathBuf {
    let d = std::env::temp_dir().join(format!("sdsp-verify-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    write_wav(&d.join("bass.wav"), &tones(&(1..=6).map(|k| (55.0 * k as f64, 0.4 / k as f64)).collect::<Vec<_>>()));
    write_wav(&d.join("chord.wav"), &tones(&[(300.0, 0.08), (450.0, 0.08), (600.0, 0.08)]));
    std::fs::write(d.join("before.toml"), BEFORE).unwrap();
    std::fs::write(d.join("after.toml"), BEFORE.replace("input = \"chord.wav\"\n", &format!("input = \"chord.wav\"\n{CUT_300}"))).unwrap();
    std::fs::write(d.join("same.toml"), BEFORE).unwrap();
    let log = [run_line("before", 0.72, -31.8), run_line("after", 0.30, -32.9), run_line("same-a", 0.72, -31.8), run_line("same-b", 0.72, -31.8)].join("\n") + "\n";
    std::fs::write(d.join("mix-log.jsonl"), log).unwrap();
    d
}

fn verify(d: &Path, before: &str, after: &str, extra: &[&str]) -> (i32, Value, String) {
    let out = Command::new(chain_bin())
        .arg("verify")
        .arg(d.join(before))
        .arg(d.join(after))
        .args(["--mix-log"])
        .arg(d.join("mix-log.jsonl"))
        .args(extra)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let json = serde_json::from_str(&text).unwrap_or(Value::Null);
    (out.status.code().unwrap_or(-1), json, text)
}

#[test]
fn verify_controls_and_table() {
    let d = fixture();

    // KEEP: the bus fell 42 pt, the stems' separation at 250-340 Hz grew ~6 dB, the bass did not move there.
    let (code, res, _) = verify(&d, "before.toml", "after.toml", &["--labels", "before,after", "--json"]);
    assert_eq!(res["verdict"], "KEEP", "{}", res["reasons"]);
    assert_eq!(code, 0);
    let bus_delta = res["bus"]["reading"]["delta"].as_f64().unwrap();
    assert!((bus_delta + 0.42).abs() < 1e-6, "{bus_delta}");
    let rd = res["render"]["reading"]["delta"].as_f64().unwrap();
    assert!(rd > 4.0 && rd < 8.0, "{rd}");
    let band = |side: &str, t: &str| res["render"][side]["stems"][t]["band_db"].as_f64().unwrap();
    let (chord_d, bass_d) = (band("after", "chord") - band("before", "chord"), band("after", "bass") - band("before", "bass"));
    assert!(chord_d > -8.0 && chord_d < -4.0 && bass_d.abs() < 0.2, "{chord_d} {bass_d}");
    assert_eq!(res["guards"].as_array().unwrap().len(), 0);

    // UNKNOWN: nothing changed on either instrument.
    let (code, res, _) = verify(&d, "before.toml", "same.toml", &["--labels", "same-a,same-b", "--json"]);
    assert_eq!(res["verdict"], "UNKNOWN");
    assert_eq!(code, 2);
    assert!(res["reasons"].as_array().unwrap().iter().any(|r| r.as_str().unwrap().contains("no change on either instrument")));
    assert!(res["render"]["reading"]["delta"].as_f64().unwrap().abs() < 1e-9);

    // UNDO: the bus claims a win the render cannot see.
    let (code, res, _) = verify(&d, "before.toml", "same.toml", &["--labels", "before,after", "--json"]);
    assert_eq!(res["verdict"], "UNDO");
    assert_eq!(code, 1);
    assert!(res["reasons"].as_array().unwrap().iter().any(|r| r.as_str().unwrap().contains("disagree")));
    assert_eq!((res["bus"]["reading"]["move"].as_i64(), res["render"]["reading"]["move"].as_i64()), (Some(1), Some(0)));

    // No labels: never KEEP on one instrument.
    let (_, res, _) = verify(&d, "before.toml", "after.toml", &["--json"]);
    assert_eq!(res["verdict"], "UNKNOWN");
    assert!(res["reasons"][0].as_str().unwrap().contains("one instrument is not an argument"));

    // Mono has no bus instrument.
    let (_, res, _) = verify(&d, "before.toml", "after.toml", &["--labels", "before,after", "--question", "mono", "--json"]);
    assert_eq!(res["verdict"], "UNKNOWN");
    assert!(res["reasons"][0].as_str().unwrap().contains("mix_phase"));

    // The table form prints both instruments and the verdict; the JSON thresholds are the documented ones.
    let (code, _, text) = verify(&d, "before.toml", "after.toml", &["--labels", "before,after"]);
    assert_eq!(code, 0);
    assert!(text.contains("instrument 1") && text.contains("instrument 2") && text.contains("VERDICT: KEEP"), "{text}");
    let (_, res, _) = verify(&d, "before.toml", "after.toml", &["--labels", "before,after", "--json"]);
    assert_eq!(res["thresholds"], serde_json::json!({"level_db": 1.0, "band_db": 1.5, "share": 0.1}));
    let _ = std::fs::remove_dir_all(&d);
}
