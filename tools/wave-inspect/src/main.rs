//! Diagnostic CLI for Wave's WAV-to-wavetable extraction.
//!
//! Takes a WAV file path, runs the same pipeline the plugin would
//! (mono-fold, pitch detect, multi-frame cycle extract, normalise),
//! prints the result, and writes the extracted frames + a 1-second
//! synthesis preview to `/tmp/` so you can listen and visually
//! inspect what landed on the canvas.
//!
//! Usage:
//!   cargo run --release -p wave-inspect -- /path/to/your.wav
//!   cargo run --release -p wave-inspect              # defaults to ~/Music/kubiz1000.wav
//!
//! Outputs (per run):
//!   /tmp/wave-inspect/<basename>__frame_a.wav   ← extracted cycle A
//!   /tmp/wave-inspect/<basename>__frame_b.wav   ← extracted cycle B (if two-frame)
//!   /tmp/wave-inspect/<basename>__preview.wav   ← 1 s of synth output at A4

use std::path::{Path, PathBuf};

const WT_SIZE: usize = 2048;
const PREVIEW_DUR_SEC: f32 = 1.0;
const PREVIEW_HZ: f32 = 440.0; // A4
const PREVIEW_SR: u32 = 44100;
const PEAK_TARGET: f32 = 0.95;

/// Spectrum-diff thresholds used to label transforms in the report.
/// Below `PHASE_ONLY` the transform changed nothing audible on steady
/// tones (mirror, invert — phase-only); below `SUBTLE` it's a small
/// shift; above is clearly audible.
const AUDIBILITY_PHASE_ONLY_DB: f32 = 5.0;
const AUDIBILITY_SUBTLE_DB: f32 = 30.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let inputs: Vec<PathBuf> = if args.is_empty() {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        vec![home.join("Music/kubiz1000.wav")]
    } else {
        args.iter().map(PathBuf::from).collect()
    };
    for input in &inputs {
        if !input.exists() {
            eprintln!("✗ file not found: {}", input.display());
            std::process::exit(1);
        }
    }
    let total = inputs.len();
    for (idx, input) in inputs.iter().enumerate() {
        if total > 1 {
            println!();
            println!("########################################################################");
            println!("# [{}/{}] {}", idx + 1, total, input.display());
            println!("########################################################################");
        }
        process_one(input);
    }
}

fn process_one(input: &std::path::Path) {

    let out_dir = PathBuf::from("/tmp/wave-inspect");
    std::fs::create_dir_all(&out_dir).unwrap();
    let basename = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("input");

    println!("══ Wave Inspect ════════════════════════════════════════════════");
    println!("input:  {}", input.display());
    println!("output: {}", out_dir.display());
    println!();

    // Stage 1 — parse the WAV file.
    let data = match superduper_synth_core::wav::parse_wav_file(&input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("✗ WAV parse failed: {e}");
            std::process::exit(2);
        }
    };
    let frames = data.frame_count();
    let secs = frames as f32 / data.sample_rate as f32;
    println!("=== Input WAV ===");
    println!("  sample rate:  {} Hz", data.sample_rate);
    println!("  channels:     {}", data.channels);
    println!("  frames:       {} ({:.3} s)", frames, secs);
    let raw_peak = data
        .samples
        .iter()
        .map(|s| s.abs())
        .fold(0.0f32, f32::max);
    let raw_rms = (data
        .samples
        .iter()
        .map(|s| s * s)
        .sum::<f32>()
        / data.samples.len() as f32)
        .sqrt();
    println!("  raw peak:     {:.3}", raw_peak);
    println!("  raw rms:      {:.3}", raw_rms);
    println!();

    // Mono fold.
    let mono: Vec<f32> = if data.channels >= 2 {
        (0..frames)
            .map(|i| {
                let (l, r) = data.read_stereo_at(i);
                0.5 * (l + r)
            })
            .collect()
    } else {
        data.samples.clone()
    };

    // Stage 2 — pitch detection.
    let pitch = superduper_synth_core::pitch::detect_pitch_hz(&mono, data.sample_rate);
    println!("=== Pitch detection ===");
    match pitch {
        Some(hz) => {
            let midi = 69.0 + 12.0 * (hz / 440.0).log2();
            println!("  detected:     {:.2} Hz", hz);
            println!("  midi note:    {:.2}  (≈{} on the keyboard)", midi, midi.round() as i32);
            let period = data.sample_rate as f32 / hz;
            println!("  period:       {:.1} samples", period);
            println!("  cycles in input: {:.1}", frames as f32 / period);
        }
        None => println!("  no pitch detected → unpitched fallback path"),
    }
    println!();

    // Stage 3 — multi-frame extraction.
    println!("=== Multi-frame extraction (n=2) ===");
    let multi = superduper_synth_core::pitch::wav_to_multi_frame(
        &mono,
        data.sample_rate,
        2,
        WT_SIZE,
        PEAK_TARGET,
    );
    let (frame_a, frame_b_opt, mode) = match multi {
        Some(ex) => {
            println!("  ✓ extracted 2 frames");
            for (i, f) in ex.frames.iter().enumerate() {
                let peak = f.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                let rms = (f.iter().map(|s| s * s).sum::<f32>() / f.len() as f32).sqrt();
                println!("    frame {i}: peak={:.3} rms={:.3} len={}", peak, rms, f.len());
            }
            let mut it = ex.frames.into_iter();
            let a = it.next().unwrap();
            let b = it.next().unwrap();
            (a, Some(b), "two-frame")
        }
        None => {
            println!("  not enough cycles for 2 frames → single-cycle fallback");
            let ex = superduper_synth_core::pitch::wav_to_single_cycle(
                &mono,
                data.sample_rate,
                WT_SIZE,
                PEAK_TARGET,
            );
            let peak = ex.curve.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
            let rms = (ex.curve.iter().map(|s| s * s).sum::<f32>() / ex.curve.len() as f32).sqrt();
            println!(
                "    fallback: peak={:.3} rms={:.3} pitched={}",
                peak, rms, ex.pitched
            );
            let mode = if ex.pitched { "single-cycle" } else { "loudest-region" };
            (ex.curve, None, mode)
        }
    };
    println!("  mode:         {mode}");
    println!();

    // Stage 4 — write frames as standalone WAVs so you can listen / inspect.
    let frame_a_path = out_dir.join(format!("{basename}__frame_a.wav"));
    superduper_synth_core::wav::write_mono_f32_wav(
        &frame_a_path,
        &frame_a,
        superduper_synth_core::wav::SINGLE_CYCLE_SAMPLE_RATE,
    )
    .unwrap();
    println!("=== Output WAVs ===");
    println!("  frame_a:      {}", frame_a_path.display());
    if let Some(ref b) = frame_b_opt {
        let frame_b_path = out_dir.join(format!("{basename}__frame_b.wav"));
        superduper_synth_core::wav::write_mono_f32_wav(
            &frame_b_path,
            b,
            superduper_synth_core::wav::SINGLE_CYCLE_SAMPLE_RATE,
        )
        .unwrap();
        println!("  frame_b:      {}", frame_b_path.display());
    }

    // Stage 5 — synth preview: render PREVIEW_DUR_SEC of A4 played
    // through frame_a + frame_b morph (WT Pos sweeps 0→1 over the
    // duration) so you can hear what the wavetable sounds like.
    let n_samples = (PREVIEW_DUR_SEC * PREVIEW_SR as f32) as usize;
    let mut preview = Vec::with_capacity(n_samples);
    let phase_inc = PREVIEW_HZ / PREVIEW_SR as f32;
    let mut phase = 0.0f32;
    for i in 0..n_samples {
        let t = i as f32 / n_samples as f32;
        let pos_in_wt = phase * WT_SIZE as f32;
        let i0 = pos_in_wt.floor() as usize % WT_SIZE;
        let i1 = (i0 + 1) % WT_SIZE;
        let frac = pos_in_wt - pos_in_wt.floor();
        let a_sample = frame_a[i0] + (frame_a[i1] - frame_a[i0]) * frac;
        let b_sample = if let Some(ref b) = frame_b_opt {
            b[i0] + (b[i1] - b[i0]) * frac
        } else {
            a_sample
        };
        // Morph WT Pos 0 → 1 across the preview.
        let wt_pos = t;
        let sample = a_sample * (1.0 - wt_pos) + b_sample * wt_pos;
        preview.push(sample * 0.6); // a bit of headroom
        phase += phase_inc;
        if phase >= 1.0 {
            phase -= 1.0;
        }
    }
    let preview_path = out_dir.join(format!("{basename}__preview.wav"));
    superduper_synth_core::wav::write_mono_f32_wav(&preview_path, &preview, PREVIEW_SR).unwrap();
    println!("  preview:      {} ({:.1} s @ A4)", preview_path.display(), PREVIEW_DUR_SEC);
    println!();
    println!("listen:  afplay {}", preview_path.display());

    // Stage 6 — assert basic invariants so this doubles as a smoke test.
    assert_eq!(frame_a.len(), WT_SIZE);
    let final_peak = frame_a.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    assert!(
        (final_peak - PEAK_TARGET).abs() < 0.05,
        "frame_a peak {final_peak} drifted from target {PEAK_TARGET}"
    );
    if let Some(ref b) = frame_b_opt {
        assert_eq!(b.len(), WT_SIZE);
        let bp = b.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        assert!((bp - PEAK_TARGET).abs() < 0.05, "frame_b peak {bp} off");
    }
    let preview_peak = preview.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    assert!(
        preview_peak > 0.1 && preview_peak < 1.0,
        "preview peak {preview_peak} should be loud but un-clipped"
    );

    // Stage 7 — apply every transform to frame_a and render a preview
    // WAV per transform. Eight derived timbres from one source —
    // the "из звука в звук" demo.
    println!();
    println!("=== Wavetable transforms (each → separate preview wav) ===");
    use superduper_synth_core::pitch as wtx;
    let transforms: Vec<(&str, Box<dyn Fn(&[f32]) -> Vec<f32>>)> = vec![
        ("mirror", Box::new(wtx::transform_mirror)),
        ("invert", Box::new(wtx::transform_invert)),
        ("octave_up", Box::new(wtx::transform_octave_up)),
        ("octave_down", Box::new(wtx::transform_octave_down)),
        ("smooth", Box::new(|f| wtx::transform_smooth(f, 11))),
        ("bright", Box::new(|f| wtx::transform_bright(f, 0.6))),
        ("phaser", Box::new(|f| wtx::transform_phase_add(f, 0.25))),
        ("foldback", Box::new(|f| wtx::transform_foldback(f, 0.7))),
        ("bitcrush_4b", Box::new(|f| wtx::transform_bitcrush(f, 4))),
        ("skew", Box::new(|f| wtx::transform_skew(f, 0.6))),
        ("sample_hold_8", Box::new(|f| wtx::transform_sample_hold(f, 8))),
    ];
    // Spectrum of the SOURCE frame_a — used to estimate how
    // audibly-distinct each transform is. Magnitude-only.
    let spec_src = superduper_synth_core::analysis::magnitude_spectrum_db(&frame_a);
    for (name, tx) in &transforms {
        let derived = tx(&frame_a);
        assert_eq!(derived.len(), WT_SIZE);
        let dpeak = derived.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        // Render a quick A4 preview of the transformed cycle.
        let mut tx_preview = Vec::with_capacity(n_samples);
        let mut tx_phase = 0.0f32;
        for _ in 0..n_samples {
            let pos = tx_phase * WT_SIZE as f32;
            let i0 = pos.floor() as usize % WT_SIZE;
            let i1 = (i0 + 1) % WT_SIZE;
            let frac = pos - pos.floor();
            tx_preview
                .push((derived[i0] + (derived[i1] - derived[i0]) * frac) * 0.6);
            tx_phase += phase_inc;
            if tx_phase >= 1.0 {
                tx_phase -= 1.0;
            }
        }
        let tx_path = out_dir.join(format!("{basename}__tx_{name}.wav"));
        superduper_synth_core::wav::write_mono_f32_wav(&tx_path, &tx_preview, PREVIEW_SR).unwrap();
        let tx_rms = (tx_preview.iter().map(|s| s * s).sum::<f32>() / n_samples as f32).sqrt();
        let spec_tx = superduper_synth_core::analysis::magnitude_spectrum_db(&derived);
        let spectrum_diff_db: f32 = spec_src
            .iter()
            .zip(spec_tx.iter())
            .take(100)
            .map(|(a, b)| (a - b).abs())
            .sum();
        let audibility = match spectrum_diff_db {
            d if d < AUDIBILITY_PHASE_ONLY_DB => "phase-only (steady tones sound identical)",
            d if d < AUDIBILITY_SUBTLE_DB => "subtle",
            _ => "clearly audible",
        };
        println!(
            "  {:<14}  peak={:.3}  rms={:.3}  Δspec={:>6.1} dB  ← {audibility}",
            name,
            dpeak,
            tx_rms,
            spectrum_diff_db,
        );
        assert!(dpeak <= 1.0 + 1e-3, "transform {name} clipped: peak {dpeak}");
        assert!(tx_rms > 0.01, "transform {name} too quiet: rms {tx_rms}");
    }

    println!();
    println!("✓ all invariants OK ({} transforms tested)", transforms.len());
}

fn _unused(_: &Path) {} // shut up unused-import lints during refactors
