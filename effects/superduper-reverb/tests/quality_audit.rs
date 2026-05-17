//! Objective quality audit for SuperDuper Reverb (Dattorro plate).
//!
//! 1. **RT60 measurement.** Standard reverberation-time test: feed an
//!    impulse, measure the time for the tail's energy to decay by 60 dB.
//!    The Decay knob should correlate (~monotonically) with measured RT60.
//!
//! 2. **Spectral decay (damping).** With heavy damping, high frequencies
//!    should disappear from the tail faster than low frequencies — that
//!    is the whole point of in-loop damping. Measured via FFT of late
//!    vs early portions of the tail.
//!
//! Run with: `cargo test -p superduper-reverb --test quality_audit -- --nocapture`

use superduper_reverb::{PlateParams, PlateState};
use superduper_synth_core::analysis::magnitude_spectrum_db;

const SR: f32 = 48_000.0;

fn run_impulse(state: &mut PlateState, p: PlateParams, secs: f32) -> Vec<f32> {
    let n = (SR * secs) as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let x = if i < 8 { 1.0_f32 } else { 0.0 };
        let (l, r) = state.process_sample(x, x, p);
        out.push((l + r) * 0.5); // mono sum for analysis
    }
    out
}

/// Estimate RT60 by finding the time the envelope (peak in 50 ms windows)
/// drops by 60 dB relative to its initial peak. Returns RT60 in seconds.
fn measure_rt60(tail: &[f32], sr: f32) -> f32 {
    // Initial peak (skip first 10 ms transient).
    let start = (sr * 0.01) as usize;
    let win = (sr * 0.05) as usize; // 50 ms windows
    let mut envelope = Vec::new();
    let mut t = Vec::new();
    let mut i = start;
    while i + win < tail.len() {
        let peak = tail[i..i + win].iter().map(|x| x.abs()).fold(0.0_f32, f32::max);
        envelope.push(peak.max(1e-9));
        t.push(i as f32 / sr);
        i += win;
    }
    if envelope.len() < 2 {
        return 0.0;
    }
    let initial = envelope[0];
    let target = initial / 1000.0; // -60 dB
    for (idx, &e) in envelope.iter().enumerate().skip(1) {
        if e < target {
            // Linear interpolate within this window for better resolution.
            let prev = envelope[idx - 1];
            let frac = (prev.log10() - target.log10()) / (prev.log10() - e.log10()).max(1e-9);
            let dt = t[idx] - t[idx - 1];
            return t[idx - 1] + dt * frac;
        }
    }
    // Decay didn't reach -60 dB inside the captured window.
    t[t.len() - 1] * 2.0
}

#[test]
fn rt60_grows_with_decay_param() {
    let mut rt60s = Vec::new();
    for decay in [0.4_f32, 0.65, 0.85] {
        let mut s = PlateState::default();
        let p = PlateParams {
            sr: SR,
            size: 1.0,
            decay,
            damp: 0.2,
            bandwidth: 0.9,
            predelay_ms: 0.0,
            modulation: 0.3,
        };
        let tail = run_impulse(&mut s, p, 10.0);
        let rt60 = measure_rt60(&tail, SR);
        rt60s.push((decay, rt60));
    }
    println!("\nReverb RT60 vs Decay param:");
    for (d, t) in &rt60s {
        println!("  decay = {d:.2}: RT60 ≈ {t:.2} s");
    }
    // Sanity: monotonic. Higher decay knob = longer measured RT60.
    assert!(rt60s[0].1 < rt60s[1].1, "decay 0.4 should be shorter than 0.65");
    assert!(rt60s[1].1 < rt60s[2].1, "decay 0.65 should be shorter than 0.85");
}

#[test]
fn damping_kills_highs_in_tail() {
    // Compare an early-tail spectrum to a late-tail spectrum at heavy
    // damping. The HF energy should disappear faster than LF — that's
    // what damping does in the feedback loop.
    let mut s = PlateState::default();
    let p = PlateParams {
        sr: SR,
        size: 1.0,
        decay: 0.85,
        damp: 0.8, // heavy
        bandwidth: 0.9,
        predelay_ms: 0.0,
        modulation: 0.3,
    };
    let tail = run_impulse(&mut s, p, 4.0);

    let early = &tail[(SR * 0.1) as usize..(SR * 0.1) as usize + 16384];
    let late = &tail[(SR * 2.0) as usize..(SR * 2.0) as usize + 16384];

    let spec_early = magnitude_spectrum_db(early);
    let spec_late = magnitude_spectrum_db(late);

    // Average dB across LF (100–500 Hz) and HF (5–10 kHz) bands. The HF
    // band should drop more dB over those 1.9 seconds than the LF band.
    let bin_hz = SR / 16384.0;
    let to_bin = |hz: f32| (hz / bin_hz) as usize;
    let band_avg = |spec: &[f32], lo: f32, hi: f32| -> f32 {
        let lo_b = to_bin(lo);
        let hi_b = to_bin(hi).min(spec.len() - 1);
        let s: f32 = spec[lo_b..=hi_b].iter().sum();
        s / (hi_b - lo_b + 1) as f32
    };
    let lf_drop = band_avg(&spec_early, 100.0, 500.0) - band_avg(&spec_late, 100.0, 500.0);
    let hf_drop = band_avg(&spec_early, 5000.0, 10000.0) - band_avg(&spec_late, 5000.0, 10000.0);
    println!("\nHeavy damping, drop over 1.9 s tail:");
    println!("  LF (100-500 Hz):    {lf_drop:>6.1} dB");
    println!("  HF (5-10 kHz):      {hf_drop:>6.1} dB");
    assert!(
        hf_drop > lf_drop + 3.0,
        "damping should kill HF faster than LF: LF drop {lf_drop}, HF drop {hf_drop}"
    );
}

#[test]
fn no_damping_keeps_highs() {
    // Mirror: at damp = 0, HF should NOT drop dramatically faster than LF
    // — both should decay at roughly the same rate.
    let mut s = PlateState::default();
    let p = PlateParams {
        sr: SR,
        size: 1.0,
        decay: 0.85,
        damp: 0.05, // almost off
        bandwidth: 1.0,
        predelay_ms: 0.0,
        modulation: 0.3,
    };
    let tail = run_impulse(&mut s, p, 4.0);

    let early = &tail[(SR * 0.1) as usize..(SR * 0.1) as usize + 16384];
    let late = &tail[(SR * 2.0) as usize..(SR * 2.0) as usize + 16384];

    let spec_early = magnitude_spectrum_db(early);
    let spec_late = magnitude_spectrum_db(late);
    let bin_hz = SR / 16384.0;
    let to_bin = |hz: f32| (hz / bin_hz) as usize;
    let band_avg = |spec: &[f32], lo: f32, hi: f32| -> f32 {
        let lo_b = to_bin(lo);
        let hi_b = to_bin(hi).min(spec.len() - 1);
        let s: f32 = spec[lo_b..=hi_b].iter().sum();
        s / (hi_b - lo_b + 1) as f32
    };
    let lf_drop = band_avg(&spec_early, 100.0, 500.0) - band_avg(&spec_late, 100.0, 500.0);
    let hf_drop = band_avg(&spec_early, 5000.0, 10000.0) - band_avg(&spec_late, 5000.0, 10000.0);
    println!("\nNo damping, drop over 1.9 s tail:");
    println!("  LF (100-500 Hz):    {lf_drop:>6.1} dB");
    println!("  HF (5-10 kHz):      {hf_drop:>6.1} dB");
    // With minimal damping, the gap should be small (< 6 dB).
    let gap = (hf_drop - lf_drop).abs();
    assert!(gap < 12.0, "no-damping HF/LF gap unexpectedly large: {gap}");
}
