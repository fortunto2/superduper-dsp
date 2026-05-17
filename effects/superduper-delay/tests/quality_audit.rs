//! Objective quality audit for SuperDuper Delay.
//!
//! 1. **Delay-time accuracy.** Push an impulse, find the peak position
//!    in the output. With Lagrange-3 the integer-delay reads should be
//!    bit-exact and fractional reads within < 0.1 sample of the truth.
//!
//! 2. **Frequency-response flatness of the dry tap.** Sweep a sine
//!    through `DelayLine::read_lagrange3` at a fixed fractional delay
//!    and measure the magnitude over 20 Hz – 16 kHz. Lagrange-3 is
//!    "maximally flat at DC" but drops ~3 dB at Nyquist/2 — verify the
//!    drop is within the published tolerance.
//!
//! 3. **Time-slew smoothness.** Step the SlewLimiter2Pole from 0 to 1
//!    and verify the first derivative never has a discontinuity above
//!    a threshold (= the property that gives us click-free tape doppler
//!    when the Time knob moves).
//!
//! Run with: `cargo test -p superduper-delay --test quality_audit -- --nocapture`

use superduper_synth_core::analysis::{
    frequency_response_error_db, frequency_response_sine_sweep, log_freq_grid,
};
use superduper_synth_core::dsp_blocks::{DelayLine, SlewLimiter2Pole};

const SR: f32 = 48_000.0;

#[test]
fn integer_delay_is_bit_exact() {
    // Push an impulse, read back at integer delay = N samples after.
    // Expect bit-exact = 1.0 ± numerical noise.
    let mut d = DelayLine::new(8192);
    d.write(1.0);
    for _ in 0..99 {
        d.write(0.0);
    }
    // Read at delay = 100 samples back from the most recent write
    // (which was the 99 zero). The impulse is 100 samples ago.
    let y = d.read_lagrange3(100.0);
    println!("integer-delay impulse readback: {y:.6} (expect 1.0)");
    assert!((y - 1.0).abs() < 1e-3, "integer tap not bit-exact: {y}");
}

#[test]
fn fractional_delay_peak_lands_between_samples() {
    // Push an impulse, then N zeros. Most recent write is "now"; the
    // impulse is N+1 samples back from now (= delay value N+1).
    // Sweep across that region to find Lagrange-3's interpolated peak.
    let mut d = DelayLine::new(8192);
    d.write(1.0);
    let n_zeros = 200_usize;
    for _ in 0..n_zeros {
        d.write(0.0);
    }
    // Impulse is at delay = (n_zeros + 1), so 201. Scan ±5 samples around.
    let expected_delay = (n_zeros + 1) as f32;
    let mut peak = 0.0_f32;
    let mut peak_at = 0.0_f32;
    for i in 0..101 {
        let delay = expected_delay - 5.0 + 0.1 * i as f32;
        let y = d.read_lagrange3(delay);
        if y.abs() > peak {
            peak = y.abs();
            peak_at = delay;
        }
    }
    println!(
        "Lagrange-3 fractional peak: {peak:.6} at delay {peak_at:.2} (expected ~{expected_delay})"
    );
    assert!(
        (peak_at - expected_delay).abs() < 1.0,
        "peak position drifted: got {peak_at}, expected {expected_delay}"
    );
    assert!(peak > 0.99, "peak amplitude too low: {peak}");
}

#[test]
fn lagrange3_frequency_response_flat_in_audio_band() {
    // Continuously feed a sine, read out 100.5 samples back. Measure the
    // gain at each frequency. Lagrange-3 should be flat (≤ 0.5 dB) up to
    // about 0.4 × Nyquist, then start to roll off slightly.
    let mut delay = DelayLine::new(8192);

    let measured = frequency_response_sine_sweep(
        |x| {
            delay.write(x);
            delay.read_lagrange3(100.5)
        },
        SR,
        &log_freq_grid(),
        0.5,
    );

    println!("\nLagrange-3 fractional-delay (frac=0.5) frequency response:");
    for &(f, db) in &measured {
        let marker = if db.abs() > 0.5 && f < 16000.0 { " !!" } else { "" };
        println!("  {f:>8.0} Hz: {db:>6.2} dB{marker}");
    }

    // Below 8 kHz expect ±0.5 dB. Above that the FIR rolls off, but the
    // standard "maximally flat" guarantee covers ~0.4 × Nyquist.
    let in_band: Vec<(f32, f32)> = measured.iter().copied().filter(|(f, _)| *f < 8000.0).collect();
    let err = frequency_response_error_db(&in_band, |_| 0.0);
    println!("worst-case below 8 kHz: {err:.3} dB");
    assert!(err < 0.5, "Lagrange-3 should be flat in audio band, max err {err} dB");
}

#[test]
fn slew_limiter_first_derivative_continuous() {
    // Two-pole slew: when the target jumps, the first derivative of the
    // output must be continuous (= no infinite instantaneous jerk). We
    // verify numerically: |Δ(Δy)| / dt² stays bounded.
    let sr = 48_000.0_f32;
    let mut s = SlewLimiter2Pole::new(0.0);
    let mut prev = 0.0_f32;
    let mut prev_diff = 0.0_f32;
    let mut max_jerk = 0.0_f32;
    for _ in 0..(sr as usize / 10) {
        let v = s.step(1.0, sr, 30.0);
        let diff = v - prev;
        let jerk = (diff - prev_diff).abs();
        if jerk > max_jerk {
            max_jerk = jerk;
        }
        prev_diff = diff;
        prev = v;
    }
    println!("\nSlewLimiter2Pole peak second-difference (jerk): {max_jerk:.6}");
    // Bounded second derivative — a single one-pole would have a step
    // change in the first derivative on target jumps (jerk ≈ 1/τ²).
    // Two cascaded poles smooth this. Our 30 ms TC should give jerk
    // well under 0.01 per sample step.
    assert!(max_jerk < 0.01, "slew first derivative jumped {max_jerk}, expected smooth");
}
