//! `sdsp-mash analyze <wav>…` — per-file tempo / meter / structure + level,
//! so a `mash.toml` can be filled in without a separate Python pass.
//!
//! Tempo is consolidated on the ported STFT-flux detector ([`crate::beats`]),
//! which also yields **downbeats** (bar starts — the beats a vocal phrase
//! should land on) and the **meter**. [`crate::tempo`]'s autocorrelation is the
//! fallback if the flux tracker returns nothing. [`crate::structure`] segments
//! the song into intro/verse/chorus/outro for cypher verse-cutting.

use std::path::Path;

use crate::beat_types::BeatConfig;
use crate::beats::{detect_beats, stft_magnitudes};
use crate::structure::detect_structure;
use crate::tempo::estimate_bpm;
use crate::wav_io::decode_any;

fn lin_to_dbfs(x: f32) -> f32 {
    20.0 * x.max(1e-9).log10()
}

fn peak_rms(l: &[f32], r: &[f32]) -> (f32, f32) {
    let mut peak = 0.0f32;
    let mut sq = 0.0f64;
    let n = l.len().min(r.len());
    for i in 0..n {
        peak = peak.max(l[i].abs()).max(r[i].abs());
        sq += (l[i] as f64) * (l[i] as f64) + (r[i] as f64) * (r[i] as f64);
    }
    let rms = if n > 0 { (sq / (2.0 * n as f64)).sqrt() as f32 } else { 0.0 };
    (peak, rms)
}

pub fn run_analyze(paths: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if paths.is_empty() {
        return Err("analyze needs at least one WAV path".into());
    }
    for p in paths {
        let w = decode_any(Path::new(p))?;
        let secs = w.frames() as f64 / w.sample_rate as f64;
        let mono: Vec<f32> = (0..w.frames()).map(|i| 0.5 * (w.l[i] + w.r[i])).collect();

        // --- Tempo / meter / downbeats (beats.rs; tempo.rs fallback) ------
        let cfg = BeatConfig::default();
        let br = detect_beats(&mono, w.sample_rate, &cfg);
        let (peak, rms) = peak_rms(&w.l, &w.r);

        println!("── analyze: {p} ──");
        println!(
            "  {} Hz   {:.1} s   {} ch",
            w.sample_rate,
            secs,
            if w.l == w.r { 1 } else { 2 }
        );

        if br.bpm > 0.0 && !br.beats.is_empty() {
            println!(
                "  BPM  {:.2}   meter {}/4   {} beats   {} downbeats",
                br.bpm,
                br.beats_per_bar,
                br.beats.len(),
                br.downbeats.len()
            );
            if let Some(&first) = br.downbeats.first() {
                let dbs: Vec<String> =
                    br.downbeats.iter().take(4).map(|t| format!("{t:.2}")).collect();
                println!("  first downbeat {first:.3} s   bars at: {} …", dbs.join(", "));
            }
        } else {
            let bf = estimate_bpm(&w.l, &w.r, w.sample_rate);
            println!(
                "  BPM  {:.2} (autocorr fallback, strength {:.2})   ÷2 {:.2} ({:.2})  ×2 {:.2} ({:.2})",
                bf.bpm, bf.strength, bf.half_bpm, bf.half_strength, bf.double_bpm, bf.double_strength
            );
        }

        // --- Structure: intro / verse / chorus / outro --------------------
        let mut planner = realfft::RealFftPlanner::<f32>::new();
        let mags = stft_magnitudes(&mono, cfg.n_fft, cfg.hop_length, &mut planner);
        let sections = detect_structure(&mags, w.sample_rate, cfg.hop_length, 64, 8.0);
        if !sections.is_empty() {
            println!("  structure ({} sections):", sections.len());
            for s in &sections {
                println!(
                    "    {:<7} {:>6.1}–{:<6.1} s  ({:>5.1} s)  energy {:.2}",
                    s.label,
                    s.start,
                    s.end,
                    s.end - s.start,
                    s.mean_energy
                );
            }
            let verses: Vec<_> = sections.iter().filter(|s| s.label == "verse").collect();
            if !verses.is_empty() {
                println!("  verse cuts (for [[track]] vocal — start_sec / len_sec):");
                for v in verses {
                    println!(
                        "    start_sec = {:.1}   len_sec = {:.1}",
                        v.start,
                        v.end - v.start
                    );
                }
            }
        }

        println!(
            "  peak {:>6.1} dBFS   RMS {:>6.1} dBFS",
            lin_to_dbfs(peak),
            lin_to_dbfs(rms)
        );
        println!();
    }
    Ok(())
}
