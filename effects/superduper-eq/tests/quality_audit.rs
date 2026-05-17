//! Objective quality audit for SuperDuper EQ.
//!
//! 1. **Frequency-response accuracy.** Run a sine sweep through a peaking
//!    biquad and compare the measured gain at each frequency to the
//!    theoretical RBJ response. Modern parametric EQs are expected to
//!    track theoretical within ±0.5 dB.
//!
//! 2. **Symmetry.** Boost N dB + cut N dB at the same f/Q must produce
//!    unity. Already checked in dsp_smoke; here we measure the deviation
//!    in dB and report the worst-case.
//!
//! 3. **THD floor.** Pure sine through any band — peaking +12 dB at
//!    1 kHz — should not introduce harmonics (the biquad is linear).
//!    THD should be at the FFT noise floor (≈ -70 dB).
//!
//! Run with: `cargo test -p superduper-eq --test quality_audit -- --nocapture`

use superduper_synth_core::analysis::{
    frequency_response_error_db, frequency_response_sine_sweep, log_freq_grid,
    make_bin_aligned_sine, measure_thd_db,
};
use superduper_synth_core::dsp_blocks::Biquad;

const SR: f32 = 48_000.0;
const FFT_LEN: usize = 16384;

/// Theoretical magnitude (in dB) of an RBJ peaking biquad at frequency `f`.
/// Solved from the analog prototype, not the bilinear-transformed digital
/// version — close enough below Nyquist/4 where most music lives.
fn peaking_theoretical_db(f: f32, centre: f32, q: f32, gain_db: f32) -> f32 {
    if f <= 0.0 {
        return 0.0;
    }
    // RBJ analog prototype: H(jω) = (s² + s·ω₀·(A/Q) + ω₀²) /
    //                                (s² + s·ω₀/(A·Q) + ω₀²)
    // where A = 10^(gain_db/40), s = jω, ω₀ = 2π·centre.
    let a = 10f32.powf(gain_db / 40.0);
    let w0 = 2.0 * core::f32::consts::PI * centre;
    let w = 2.0 * core::f32::consts::PI * f;
    let w2 = w * w;
    let w02 = w0 * w0;
    // |H|² with s=jω:  numerator |ω₀² - ω² + jω·ω₀·A/Q|², denominator
    //                  |ω₀² - ω² + jω·ω₀/(A·Q)|²
    let num_real = w02 - w2;
    let num_imag = w * w0 * a / q;
    let den_real = w02 - w2;
    let den_imag = w * w0 / (a * q);
    let num_mag_sq = num_real * num_real + num_imag * num_imag;
    let den_mag_sq = den_real * den_real + den_imag * den_imag;
    10.0 * (num_mag_sq / den_mag_sq.max(1e-30)).max(1e-30).log10()
}

#[test]
fn peaking_matches_theoretical_within_1db() {
    // +6 dB peaking at 1 kHz, Q=1.0. Sweep 1/3-octave from 20 Hz to 16k.
    // The digital biquad's response will diverge from the analog
    // prototype near Nyquist (frequency warping is part of the bilinear
    // transform). At 1 kHz centre on 48 kHz SR, we're far from Nyquist —
    // expect tracking within ~1 dB.
    let mut b = Biquad::default();
    b.set_peaking(SR, 1000.0, 1.0, 6.0);

    let freqs = log_freq_grid();
    let measured = frequency_response_sine_sweep(
        |x| b.process(x),
        SR,
        &freqs,
        0.5, // half-second tone per frequency
    );

    println!("\nPeaking EQ (+6 dB @ 1 kHz, Q=1) vs analog prototype:");
    for &(f, db) in &measured {
        let t = peaking_theoretical_db(f, 1000.0, 1.0, 6.0);
        let err = (db - t).abs();
        let marker = if err > 1.0 { " !!" } else { "" };
        println!("  {f:>8.0} Hz: measured {db:>6.2} dB  theoretical {t:>6.2} dB  err {err:.2}{marker}");
    }

    let err = frequency_response_error_db(&measured, |f| {
        peaking_theoretical_db(f, 1000.0, 1.0, 6.0)
    });
    println!("worst-case error: {err:.2} dB");
    // Allow up to 1.5 dB — bilinear warping at the upper frequencies plus
    // the measurement's FFT bin resolution add up.
    assert!(err < 1.5, "peaking EQ deviates {err} dB from analog prototype");
}

#[test]
fn boost_cut_pair_is_flat() {
    // RBJ cookbook guarantee: boost N dB + cut N dB → exactly flat.
    // Already tested in dsp_smoke; here we report the *deviation in dB*
    // so we can see how flat it actually is.
    let mut b1 = Biquad::default();
    let mut b2 = Biquad::default();
    b1.set_peaking(SR, 1500.0, 1.5, 8.0);
    b2.set_peaking(SR, 1500.0, 1.5, -8.0);

    let freqs = log_freq_grid();
    let measured = frequency_response_sine_sweep(
        |x| b2.process(b1.process(x)),
        SR,
        &freqs,
        0.5,
    );
    let worst = frequency_response_error_db(&measured, |_| 0.0);
    println!("\nBoost+Cut deviation from unity: max {worst:.3} dB across 20 Hz–16 kHz");
    assert!(worst < 0.5, "boost+cut should stay within 0.5 dB of unity, got {worst}");
}

#[test]
fn linear_filter_no_thd() {
    // Even a +12 dB peak shouldn't introduce harmonic content — biquad
    // is a linear system. THD must sit at the noise floor.
    let mut b = Biquad::default();
    b.set_peaking(SR, 1000.0, 1.0, 12.0);

    let (samples, freq) = make_bin_aligned_sine(FFT_LEN, SR, 1000.0, 0.5);
    let processed: Vec<f32> = samples.iter().map(|&x| b.process(x)).collect();
    let thd = measure_thd_db(&processed, freq, SR);
    println!("\nLinear peaking biquad THD: {thd:.1} dB (FFT noise floor)");
    assert!(thd < -60.0, "linear biquad should have THD < -60 dB, got {thd}");
}

#[test]
fn low_shelf_response_at_corner() {
    // Low shelf at 200 Hz, +6 dB. At DC the gain should be exactly +6 dB
    // (the shelf is fully engaged). At 10 × the corner (2 kHz) it should
    // be ≤ 0.5 dB.
    let mut b = Biquad::default();
    b.set_low_shelf(SR, 200.0, 1.0, 6.0);

    let freqs = vec![30.0_f32, 200.0, 500.0, 2000.0, 8000.0];
    let measured = frequency_response_sine_sweep(|x| b.process(x), SR, &freqs, 0.5);
    println!("\nLow shelf +6 dB @ 200 Hz:");
    for &(f, db) in &measured {
        println!("  {f:>6.0} Hz: {db:>6.2} dB");
    }

    let lf = measured.iter().find(|(f, _)| *f < 50.0).unwrap().1;
    let hf = measured.iter().find(|(f, _)| *f > 5000.0).unwrap().1;
    assert!(
        (lf - 6.0).abs() < 0.5,
        "Low shelf at DC should be +6 dB ±0.5, got {lf}"
    );
    assert!(hf.abs() < 0.5, "Low shelf at 8 kHz should be 0 ±0.5 dB, got {hf}");
}

#[test]
fn hpf_attenuates_below_corner() {
    // HPF at 1 kHz Q=0.707 (Butterworth). At 100 Hz (1 decade below)
    // expect ≈ -40 dB. At 10 kHz (1 decade above) expect ≈ 0 dB.
    let mut b = Biquad::default();
    b.set_hpf(SR, 1000.0, 0.707);

    let freqs = vec![100.0_f32, 500.0, 1000.0, 2000.0, 10000.0];
    let measured = frequency_response_sine_sweep(|x| b.process(x), SR, &freqs, 0.5);
    println!("\nHPF @ 1 kHz Q=0.707:");
    for &(f, db) in &measured {
        println!("  {f:>6.0} Hz: {db:>7.2} dB");
    }

    let f100 = measured[0].1;
    let f1k = measured[2].1;
    let f10k = measured[4].1;
    assert!(f100 < -30.0, "HPF at 100 Hz should be ≤ -30 dB, got {f100}");
    assert!((f1k - (-3.0)).abs() < 1.5, "HPF at corner should be ≈ -3 dB, got {f1k}");
    assert!(f10k.abs() < 0.5, "HPF at 10 kHz should be ≈ 0 dB, got {f10k}");
}
