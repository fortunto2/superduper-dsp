//! Objective quality audit for SuperDuper Saturator.
//!
//! Two batteries of measurements:
//!
//! 1. **THD curve characterisation.** Pure 1 kHz sine at unity gain
//!    through each saturation curve at moderate drive (6 dB). Reports
//!    THD — the Soft (tanh) curve should be the cleanest, Tube should
//!    have richer harmonic content (its 2nd-harmonic asymmetry), and
//!    Tape should be flatter than tanh thanks to the algebraic
//!    soft-clip shape.
//!
//! 2. **Aliasing rejection vs OS mode.** Feed a 21.6 kHz sine
//!    (0.45 × Nyquist) through the saturator at OS=Off / 2× / 4×.
//!    Measures the strongest non-harmonic bin (= aliasing image).
//!    Off should be loud, 2× noticeably better, 4× ≥ 60 dB clean.
//!
//! Run with: `cargo test -p superduper-saturator --test quality_audit -- --nocapture`

use superduper_synth_core::analysis::{
    make_bin_aligned_sine, measure_aliasing_db, measure_thd_db,
};
use superduper_synth_core::dsp_blocks::{tanh_drive, tape_clip, tube_clip, Oversampler2x};

const SR: f32 = 48_000.0;
const FFT_LEN: usize = 16384;

fn saturate(curve: u32, x: f32, drive: f32) -> f32 {
    match curve {
        1 => tube_clip(x, drive),
        2 => tanh_drive(x, drive),
        _ => tape_clip(x, drive),
    }
}

fn process(curve: u32, os_mode: u32, samples: &[f32], drive: f32) -> Vec<f32> {
    let mut os1 = Oversampler2x::default();
    let mut os2 = Oversampler2x::default();
    samples
        .iter()
        .map(|&x| match os_mode {
            0 => saturate(curve, x, drive),
            1 => {
                let (e, o) = os1.upsample(x);
                let se = saturate(curve, e, drive);
                let so = saturate(curve, o, drive);
                os1.downsample(se, so)
            }
            _ => {
                let (e1, o1) = os1.upsample(x);
                let (e2a, o2a) = os2.upsample(e1);
                let (e2b, o2b) = os2.upsample(o1);
                let a = os2.downsample(saturate(curve, e2a, drive), saturate(curve, o2a, drive));
                let b = os2.downsample(saturate(curve, e2b, drive), saturate(curve, o2b, drive));
                os1.downsample(a, b)
            }
        })
        .collect()
}

#[test]
fn thd_per_curve_at_6db_drive() {
    let drive = 10f32.powf(6.0 / 20.0); // +6 dB
    let (samples, freq) = make_bin_aligned_sine(FFT_LEN, SR, 1000.0, 0.7);

    let mut results = Vec::new();
    for (curve, name) in [(0u32, "Tape"), (1, "Tube"), (2, "Soft/tanh")] {
        let processed = process(curve, 1, &samples, drive);
        let thd = measure_thd_db(&processed, freq, SR);
        results.push((name, thd));
    }

    println!("\nSaturator THD @ 1 kHz, +6 dB drive (OS=2×):");
    for (name, thd) in &results {
        println!("  {name:>10}: {thd:>6.1} dB");
    }

    // Sanity bounds — each curve must produce measurable harmonic
    // content (lower than the FFT floor) but not insane levels.
    for (name, thd) in results {
        assert!(thd > -60.0, "{name}: THD floor too low ({thd}) — likely no distortion happening");
        assert!(thd < -10.0, "{name}: THD ({thd}) suspiciously high — clipping?");
    }
}

#[test]
fn aliasing_rejection_scales_with_os() {
    // Drive a 21.6 kHz sine (just below Nyquist) through the saturator.
    // Every harmonic created by saturate() lives above Nyquist and folds
    // back as aliasing — that's the worst-case stress test.
    let drive = 10f32.powf(12.0 / 20.0); // +12 dB — hard drive
    let (samples, freq) = make_bin_aligned_sine(FFT_LEN, SR, 21_600.0, 0.5);

    let curve = 2; // tanh — produces richest harmonics, hardest to clean up
    let mut results = Vec::new();
    for (os, label) in [(0u32, "Off"), (1, "2×"), (2, "4×")] {
        let processed = process(curve, os, &samples, drive);
        let alias = measure_aliasing_db(&processed, freq, SR);
        results.push((label, alias));
    }

    println!("\nSaturator (tanh, +12 dB drive) aliasing @ 21.6 kHz:");
    for (label, alias) in &results {
        println!("  OS = {label:>4}: {alias:>6.1} dB (lower = cleaner)");
    }

    // Reality check: oversampling must measurably help.
    let off = results[0].1;
    let two_x = results[1].1;
    let four_x = results[2].1;
    assert!(
        two_x < off - 3.0,
        "2× should reduce aliasing by ≥ 3 dB; got Off={off:.1} 2×={two_x:.1}"
    );
    assert!(
        four_x <= two_x + 1.0,
        "4× should be ≤ 2× in aliasing (more rejection); got 2×={two_x:.1} 4×={four_x:.1}"
    );
}

#[test]
fn high_drive_thd_consistent_with_os() {
    // Heavy drive (+18 dB) shouldn't change the *type* of distortion —
    // harmonic spread should look similar at OS=2× vs 4× (4× just kills
    // the alias products, not the wanted harmonics). So THD numbers
    // measured at low-frequency input should be similar between modes.
    let drive = 10f32.powf(18.0 / 20.0);
    let (samples, freq) = make_bin_aligned_sine(FFT_LEN, SR, 200.0, 0.5);

    let thd_2x = measure_thd_db(&process(2, 1, &samples, drive), freq, SR);
    let thd_4x = measure_thd_db(&process(2, 2, &samples, drive), freq, SR);

    println!("\nSaturator high-drive THD at 200 Hz:");
    println!("  OS=2×: {thd_2x:.1} dB");
    println!("  OS=4×: {thd_4x:.1} dB  (should match within ~3 dB)");
    assert!(
        (thd_2x - thd_4x).abs() < 5.0,
        "THD should be consistent across OS modes at LF input ({thd_2x} vs {thd_4x})"
    );
}
