//! Anti-aliasing acceptance tests for the ceiling oversampler.
//!
//! A loud sine close to Nyquist gets pushed into tanh; without OS, the
//! tanh's odd harmonics (3f, 5f, 7f) mirror around Nyquist back into the
//! audible band. With 2× / 4× OS those mirror images live above the
//! native Nyquist and the halfband decimator attenuates them by ~80 dB.
//!
//! The numbers below are conservative — they only assert "OS reduces
//! aliasing meaningfully", not specific dB targets, so the test stays
//! stable across small filter tweaks.

use superduper_synth_core::analysis::{make_bin_aligned_sine, measure_aliasing_db};
use superduper_synth_core::dsp_blocks::{oversample_apply, Oversampler2x};

const SR: f32 = 48000.0;
/// Same FFT block size the Saturator uses for its aliasing audit.
const FFT_LEN: usize = 16384;
/// Aggressive ceiling — drive ≈ +18 dB into a -3 dB tanh ceiling so the
/// soft clipper actually saturates hard enough to produce odd harmonics
/// near the fundamental amplitude (otherwise tanh's gentle curve emits
/// negligible aliasing in the first place).
const CEIL_DB: f32 = -3.0;
const INPUT_PEAK: f32 = 5.0; // +14 dBFS — tanh sees ~7.07 → fully saturated

/// Drive a bin-aligned sine close to Nyquist through the clipper at the
/// requested OS mode. Returns the FFT-ready output. Warm-up loop on top
/// kills the halfband filter transient so the FFT sees only steady-state.
fn run_clipper(os_mode: u32, samples: &[f32]) -> Vec<f32> {
    let ceil_lin = 10f32.powf(CEIL_DB / 20.0);
    let clip = |s: f32| ceil_lin * (s / ceil_lin).tanh();
    let mut os1 = Oversampler2x::default();
    let mut os2 = Oversampler2x::default();
    // Warm halfband delays so the FFT sees steady-state.
    for &x in samples.iter().take(2048) {
        let _ = oversample_apply(x * INPUT_PEAK, os_mode, &mut os1, &mut os2, clip);
    }
    samples
        .iter()
        .map(|&x| oversample_apply(x * INPUT_PEAK, os_mode, &mut os1, &mut os2, clip))
        .collect()
}

#[test]
fn aliasing_rejection_scales_with_os() {
    // Mirror of the Saturator's audit: bin-aligned 21.6 kHz sine, hard
    // clip, measure aliased peak relative to fundamental.
    let (samples, freq) = make_bin_aligned_sine(FFT_LEN, SR, 21_600.0, 0.5);
    let mut results = Vec::new();
    for (os, label) in [(0u32, "Off"), (1, "2×"), (2, "4×")] {
        let processed = run_clipper(os, &samples);
        let alias = measure_aliasing_db(&processed, freq, SR);
        results.push((label, alias));
    }
    println!("\nCompressor ceiling clipper aliasing @ 21.6 kHz, -3 dB ceiling:");
    for (label, alias) in &results {
        println!("  OS = {label:>4}: {alias:>6.1} dB (lower = cleaner)");
    }
    let off = results[0].1;
    let two_x = results[1].1;
    let four_x = results[2].1;
    assert!(
        two_x < off - 3.0,
        "2× should reduce aliasing by ≥ 3 dB; got Off={off:.1} 2×={two_x:.1}"
    );
    assert!(
        four_x <= two_x + 1.0,
        "4× should be ≤ 2× in aliasing; got 2×={two_x:.1} 4×={four_x:.1}"
    );
}

#[test]
fn os_off_passes_small_signals_unchanged() {
    let clip = |s: f32| s.tanh();
    let mut os1 = Oversampler2x::default();
    let mut os2 = Oversampler2x::default();
    for i in 0..1024 {
        let x = 0.1 * (i as f32 * core::f32::consts::TAU * 1000.0 / SR).sin();
        let _ = oversample_apply(x, 0, &mut os1, &mut os2, clip);
    }
    let x = 0.1;
    let y = oversample_apply(x, 0, &mut os1, &mut os2, clip);
    // Native rate, tanh(0.1)=0.0997 — small but non-zero distortion.
    let err = (y - x).abs() / x;
    assert!(err < 0.005, "native bypass distorts more than expected: {err}");
}
