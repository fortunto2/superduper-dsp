//! `nam-test` — autotest harness for SuperDuper NAM loader / inference.
//!
//! Scans a directory of `.nam` files (default `~/.superduper-dsp/nam/`),
//! loads each one through the same code path the plugin uses, and runs
//! four probes per model:
//!
//! 1. **Silence** — 1 second of zeros. Must produce a finite, bounded output.
//! 2. **DC step** — 0.3 for 1 second. Must reach a finite steady state.
//! 3. **Sine** — 1 kHz at 0.3 amplitude for 1 second. Must remain bounded
//!    and produce a non-trivial RMS (otherwise the model is silent).
//! 4. **Sweep** — log sweep 50 Hz → 8 kHz, 0.3 amp, 2 seconds. Catches
//!    instability that only shows up at certain frequencies.
//!
//! Prints a per-model table and exits non-zero if any probe fails. Run
//! from CI to catch regressions in the loader or inference math.

use std::path::PathBuf;
use std::process::ExitCode;

use superduper_synth_core::nam::{load_from_json, NamError, NamModel};

const SAMPLE_RATE: f32 = 48_000.0;
const SECONDS: usize = 1;
const SWEEP_SECONDS: usize = 2;

#[derive(Debug)]
struct ProbeResult {
    name: &'static str,
    max_abs: f32,
    rms: f32,
    finite: bool,
}

#[derive(Debug)]
enum LoadOutcome {
    Loaded {
        arch: &'static str,
        params: usize,
        probes: Vec<ProbeResult>,
    },
    Skipped(String),
    Failed(String),
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let dir = if let Some(d) = args.next() {
        PathBuf::from(d)
    } else {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".superduper-dsp/nam")
    };
    if !dir.is_dir() {
        eprintln!("nam-test: not a directory: {}", dir.display());
        return ExitCode::from(2);
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|_| panic!("nam-test: cannot read {}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("nam"))
        .collect();
    files.sort();

    if files.is_empty() {
        eprintln!("nam-test: no .nam files in {}", dir.display());
        return ExitCode::from(1);
    }

    println!("nam-test: scanning {} files in {}", files.len(), dir.display());
    let mut any_failed = false;
    for path in &files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        let outcome = test_one(path);
        match &outcome {
            LoadOutcome::Loaded {
                arch,
                params,
                probes,
            } => {
                println!(
                    "  [ok ] {:30} {:7} {:>6} params",
                    stem,
                    arch,
                    format!("{}", params)
                );
                for p in probes {
                    let flag = if p.finite { "·" } else { "FAIL" };
                    println!(
                        "         probe {:8} max|x|={:7.4}  rms={:7.4}  {}",
                        p.name, p.max_abs, p.rms, flag
                    );
                    if !p.finite {
                        any_failed = true;
                    }
                }
                // Heuristic: sine probe must produce a non-zero RMS.
                // A model that returns 0 for everything is structurally
                // broken, even if it doesn't NaN.
                if let Some(sine) = probes.iter().find(|p| p.name == "sine") {
                    if sine.finite && sine.rms < 1e-5 {
                        println!("         FAIL: sine probe RMS too low — model is silent");
                        any_failed = true;
                    }
                }
            }
            LoadOutcome::Skipped(reason) => {
                println!("  [skip] {:30} {}", stem, reason);
            }
            LoadOutcome::Failed(err) => {
                println!("  [FAIL] {:30} {}", stem, err);
                any_failed = true;
            }
        }
    }

    if any_failed {
        println!("\nnam-test: SOME PROBES FAILED");
        ExitCode::from(1)
    } else {
        println!("\nnam-test: all probes passed");
        ExitCode::SUCCESS
    }
}

fn test_one(path: &std::path::Path) -> LoadOutcome {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return LoadOutcome::Failed(format!("read: {}", e)),
    };
    let file = match load_from_json(&text) {
        Ok(f) => f,
        Err(e) => return LoadOutcome::Failed(format!("parse: {}", e)),
    };
    let mut model = match NamModel::from_nam_file(&file) {
        Ok(m) => m,
        Err(NamError::UnsupportedArch(arch)) => {
            return LoadOutcome::Skipped(format!("unsupported arch: {}", arch));
        }
        Err(NamError::Unsupported(what)) => {
            return LoadOutcome::Skipped(format!("unsupported feature: {}", what));
        }
        Err(e) => return LoadOutcome::Failed(format!("build: {}", e)),
    };
    let arch = model.arch_name();
    let params = model.param_count();

    let probes = vec![
        run_probe(&mut model, "silence", silence_probe()),
        run_probe(&mut model, "dc", dc_probe()),
        run_probe(&mut model, "sine", sine_probe()),
        run_probe(&mut model, "sweep", sweep_probe()),
    ];

    LoadOutcome::Loaded {
        arch,
        params,
        probes,
    }
}

fn run_probe(model: &mut NamModel, name: &'static str, input: Vec<f32>) -> ProbeResult {
    model.reset();
    let mut max_abs: f32 = 0.0;
    let mut sum_sq: f32 = 0.0;
    let mut finite = true;
    for &x in &input {
        let y = model.process(x);
        if !y.is_finite() || y.abs() > 1e6 {
            finite = false;
        }
        if y.abs() > max_abs {
            max_abs = y.abs();
        }
        sum_sq += y * y;
    }
    let rms = (sum_sq / input.len().max(1) as f32).sqrt();
    ProbeResult {
        name,
        max_abs,
        rms,
        finite,
    }
}

fn silence_probe() -> Vec<f32> {
    vec![0.0; SAMPLE_RATE as usize * SECONDS]
}

fn dc_probe() -> Vec<f32> {
    vec![0.3; SAMPLE_RATE as usize * SECONDS]
}

fn sine_probe() -> Vec<f32> {
    let n = SAMPLE_RATE as usize * SECONDS;
    let omega = 2.0 * std::f32::consts::PI * 1000.0 / SAMPLE_RATE;
    (0..n).map(|i| 0.3 * (i as f32 * omega).sin()).collect()
}

/// Log-frequency sweep 50 Hz → 8 kHz over `SWEEP_SECONDS`.
fn sweep_probe() -> Vec<f32> {
    let n = SAMPLE_RATE as usize * SWEEP_SECONDS;
    let f0 = 50.0_f32;
    let f1 = 8000.0_f32;
    let k = (f1 / f0).ln();
    let t_per_sample = 1.0 / SAMPLE_RATE;
    let mut phase = 0.0_f32;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / SAMPLE_RATE;
        let f = f0 * (k * t / SWEEP_SECONDS as f32).exp();
        phase += 2.0 * std::f32::consts::PI * f * t_per_sample;
        if phase > 2.0 * std::f32::consts::PI {
            phase -= 2.0 * std::f32::consts::PI;
        }
        out.push(0.3 * phase.sin());
    }
    out
}
