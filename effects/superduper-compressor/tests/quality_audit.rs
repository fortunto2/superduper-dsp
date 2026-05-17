//! Objective quality audit for SuperDuper Compressor.
//!
//! Strategy: drive the **shared static curve** directly (no envelope
//! follower, no lookahead, no plugin instantiation) and compare the
//! measured gain reduction at each input level to the theoretical
//! Giannoulis-Massberg-Reiss formula.
//!
//! The plugin uses `compressor_gain_db()` from synth-core/dsp_blocks
//! which is exactly that formula, so this test verifies our soft-knee
//! shape, hard-region ratio, and below-threshold linearity all match
//! their specification.
//!
//! Run with: `cargo test -p superduper-compressor --test quality_audit -- --nocapture`

use superduper_synth_core::dsp_blocks::compressor_gain_db;

/// Theoretical Giannoulis-Massberg-Reiss soft-knee static curve.
///
/// Returns gain reduction in dB (≤ 0). Formula from "Digital Dynamic
/// Range Compressor Design — A Tutorial and Analysis", JAES 2012, eq. 4.
fn theoretical_gr_db(input_db: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
    let knee_half = knee_db * 0.5;
    let slope = 1.0 - 1.0 / ratio.max(1.0);
    if knee_db > 0.0001 && (input_db - threshold_db).abs() <= knee_half {
        let x = input_db - threshold_db + knee_half;
        -(slope * x * x) / (2.0 * knee_db)
    } else if input_db > threshold_db + knee_half {
        -(input_db - threshold_db) * slope
    } else {
        0.0
    }
}

#[test]
fn static_curve_matches_giannoulis() {
    // Sweep input from -60 dB to 0 dB, compare measured curve to theoretical.
    let threshold = -18.0_f32;
    let ratio = 4.0_f32;
    let knee = 6.0_f32;

    let mut worst_err = 0.0_f32;
    let mut worst_at = 0.0_f32;
    let mut data = Vec::new();

    for db_int in -60..=0 {
        let in_db = db_int as f32;
        let measured = compressor_gain_db(in_db, threshold, ratio, knee);
        let theoretical = theoretical_gr_db(in_db, threshold, ratio, knee);
        let err = (measured - theoretical).abs();
        if err > worst_err {
            worst_err = err;
            worst_at = in_db;
        }
        data.push((in_db, measured, theoretical));
    }

    println!("\nCompressor static curve (T={threshold} dB R={ratio} K={knee} dB):");
    println!("  input  measured  theoretical  err");
    for (i, (in_db, m, t)) in data.iter().enumerate() {
        // Print every 6th sample to keep output readable.
        if i % 6 == 0 {
            println!("  {in_db:>5.0}    {m:>6.2}    {t:>6.2}        {:.3}", (m - t).abs());
        }
    }
    println!("worst-case deviation: {worst_err:.4} dB @ {worst_at} dB input");
    assert!(
        worst_err < 0.001,
        "static curve deviates from theoretical by {worst_err} dB",
    );
}

#[test]
fn below_threshold_is_zero() {
    let threshold = -20.0_f32;
    let knee = 8.0_f32;

    for db_int in -60..=(-25) {
        let gr = compressor_gain_db(db_int as f32, threshold, 4.0, knee);
        assert!(gr.abs() < 1e-4, "below knee should be 0 GR, got {gr} at {db_int} dB");
    }
}

#[test]
fn ratio_one_is_no_compression() {
    // Ratio 1:1 = unity, no GR even far above threshold.
    let threshold = -18.0_f32;
    for db_int in -30..=10 {
        let gr = compressor_gain_db(db_int as f32, threshold, 1.0, 6.0);
        assert!(gr.abs() < 1e-4, "1:1 should never compress, got {gr} at {db_int}");
    }
}

#[test]
fn hard_knee_above_threshold_follows_slope() {
    // Hard knee (= 0) → gain reduction linear with input above threshold.
    let threshold = -20.0_f32;
    let ratio = 4.0_f32;
    let slope = 1.0 - 1.0 / ratio; // 0.75

    for (in_db, expected_gr) in [
        (-10.0_f32, -10.0 * slope),
        (  0.0_f32, -20.0 * slope),
        ( -5.0_f32, -15.0 * slope),
    ] {
        let gr = compressor_gain_db(in_db, threshold, ratio, 0.0);
        let err = (gr - expected_gr).abs();
        assert!(err < 0.001, "in={in_db} expected gr={expected_gr} got {gr}");
    }
}

#[test]
fn soft_knee_monotonic() {
    // Across the knee region, GR must be monotonic non-increasing.
    let threshold = -18.0_f32;
    let knee = 10.0_f32;
    let mut prev = 0.0_f32;
    let mut last_in = -100.0_f32;
    for db_int_x10 in -300..=0 {
        let in_db = db_int_x10 as f32 / 10.0;
        let gr = compressor_gain_db(in_db, threshold, 4.0, knee);
        assert!(
            gr <= prev + 1e-4,
            "non-monotonic: prev={prev} ({last_in}) → curr={gr} ({in_db})"
        );
        prev = gr;
        last_in = in_db;
    }
    println!("soft-knee monotonicity verified over -30..0 dB");
}
