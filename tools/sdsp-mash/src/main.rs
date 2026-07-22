//! sdsp-mash — headless mashup renderer.
//!
//! Usage:
//!     sdsp-mash <mash.toml> <output.wav>   render a mashup
//!     sdsp-mash analyze <wav>…             print BPM + peak/RMS per file
//!
//! The render path reads the mashup config (stems, BPM grid, per-track
//! stretch/FX/auto-align, ducking, intro sweep, master chain), renders the
//! aligned + ducked + swept + mastered stereo mix, and writes it out with
//! per-stage LUFS-I / dBTP / RMS analysis. See `example.toml`.

use std::path::PathBuf;
use std::process::ExitCode;

use superduper_synth_core::loudness::{LoudnessMeter, TruePeakDetector};

mod align;
mod analyze;
mod beat_types;
mod beats;
mod chain;
mod config;
mod duck;
mod fx;
mod mix;
mod structure;
mod onset;
mod phrase;
mod render;
mod stretch;
mod sweep;
mod tempo;
mod track_fx;
mod wav_io;

#[cfg(test)]
mod integration_tests;

use chain::run_master;
use config::MashConfig;
use mix::mix;
use render::{prepare, role_counts};
use wav_io::write_stereo_f32_wav;

/// Print LUFS-I + dBTP + RMS for a stereo bus, sdsp-chain style.
fn measure(name: &str, l: &[f32], r: &[f32], sr: f32) {
    let mut meter = LoudnessMeter::new(sr);
    let mut tp = TruePeakDetector::new();
    for (a, b) in l.iter().zip(r.iter()) {
        meter.process_stereo(*a, *b);
        tp.process_stereo(*a, *b);
    }
    let n = (l.len() + r.len()) as f32;
    let sum_sq: f32 = l.iter().map(|x| x * x).sum::<f32>() + r.iter().map(|x| x * x).sum::<f32>();
    let rms = (sum_sq / n.max(1.0)).sqrt();
    let tp_db = tp.dbtp();
    let tp_disp = if tp_db.is_finite() {
        format!("{tp_db:>6.1}")
    } else {
        "  -inf".to_string()
    };
    println!(
        "  [{name:>14}]  LUFS-I {:>7.2}   TP {} dBTP   RMS {:.4}",
        meter.integrated_lufs(),
        tp_disp,
        rms,
    );
}

fn render_mashup(cfg_path: &PathBuf, out_path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = MashConfig::parse(&std::fs::read_to_string(cfg_path)?)?;

    println!("══ sdsp-mash ══");
    println!(
        "Config: {}   BPM {}   {} flat stems   {} sections   {} master stages",
        cfg_path.display(),
        cfg.bpm,
        cfg.tracks.len(),
        cfg.sections.len(),
        cfg.master.len()
    );
    for s in &cfg.sections {
        println!(
            "  section @ beat {:>6.1}  {:<12}  {}",
            s.start_beat,
            s.transition.as_deref().unwrap_or("-"),
            s.name.as_deref().unwrap_or("")
        );
    }

    // Warn on config combinations that silently do nothing.
    let (vocals, others) = role_counts(&cfg);
    if cfg.duck.is_some() && (vocals == 0 || others == 0) {
        eprintln!(
            "warning: [duck] set but there {} — ducking will have no audible effect",
            match (vocals, others) {
                (0, 0) => "is no vocal key and no beat-other target".to_string(),
                (0, _) => "is no vocal key".to_string(),
                (_, 0) => "is no beat-other target".to_string(),
                _ => String::new(),
            }
        );
    }

    let prep = prepare(&cfg)?;
    let sr = prep.sr;
    let sr_f = sr as f32;

    // Report any vocal auto-alignments.
    for a in &prep.align_reports {
        let verdict = if a.applied {
            format!("shift {:+.1} ms applied", a.shift_ms)
        } else {
            format!("shift {:+.1} ms rejected (low corr) — kept nominal", a.shift_ms)
        };
        println!(
            "auto-align: {}  nominal {:.0} ms → {verdict}  (corr {:.2})",
            a.path, a.nominal_ms, a.score
        );
    }

    let (mut l, mut r) = mix(&prep.stems, &prep.settings);
    if l.is_empty() {
        return Err("no stems produced any audio".into());
    }

    // Timeline effects (transitions + explicit [[fx]]) on the pre-master mix.
    if !prep.fx.is_empty() {
        println!("Timeline FX: {} events", prep.fx.len());
        fx::apply_all(&mut l, &mut r, sr, cfg.bpm, &prep.fx);
    }

    println!("\nPer-stage analysis (LUFS-Integrated + True-Peak):");
    measure("premaster", &l, &r, sr_f);

    if !cfg.master.is_empty() {
        let mut observe = |label: &str, ll: &[f32], rr: &[f32]| measure(label, ll, rr, sr_f);
        let (ml, mr) = run_master(&cfg.master, l, r, sr, &mut observe);
        l = ml;
        r = mr;
    }

    // Even out section loudness on the MASTERED output (after the limiter's
    // density response) — attenuation only, so nothing clips.
    let adj = render::level_premaster_sections(&cfg, sr, &mut l, &mut r);
    if !adj.is_empty() {
        let s: Vec<String> = adj.iter().map(|d| format!("{d:+.1}")).collect();
        println!("Section balance (dB, atten): {}", s.join("  "));
        measure("balanced", &l, &r, sr_f);
    }

    write_stereo_f32_wav(out_path, &l, &r, sr)?;
    let secs = l.len() as f32 / sr_f;
    println!(
        "\n→ Wrote {}  ({:.1}s stereo @ {} Hz)",
        out_path.display(),
        secs,
        sr
    );
    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Subcommand: analyze.
    if args.first().map(|s| s.as_str()) == Some("analyze") {
        return analyze::run_analyze(&args[1..]);
    }

    if args.len() != 2 {
        eprintln!("Usage:");
        eprintln!("  sdsp-mash <mash.toml> <output.wav>   render a mashup");
        eprintln!("  sdsp-mash analyze <wav>…             print BPM + peak/RMS");
        return Err("bad arguments".into());
    }
    render_mashup(&PathBuf::from(&args[0]), &PathBuf::from(&args[1]))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
