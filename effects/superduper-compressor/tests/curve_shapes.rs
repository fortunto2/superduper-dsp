//! Static-curve tests for the three compression shapes.
//!
//! Curves are pure functions of (input_db, threshold, ratio, knee), so
//! the test surface is small: each shape should monotonically increase
//! GR with input, Clean must match the bit-identical legacy formula,
//! Pump and Smooth must be measurably different from Clean somewhere,
//! and none of them is allowed to produce *positive* gain (we always
//! reduce or pass through, never amplify).

use superduper_synth_core::dsp_blocks::{
    compressor_gain_db, compressor_gain_db_curve, CompressorCurve,
};

const T: f32 = -18.0;
const R: f32 = 4.0;
const K: f32 = 8.0;

fn sweep(curve: CompressorCurve) -> Vec<(f32, f32)> {
    (-60..=0)
        .step_by(2)
        .map(|x| {
            let x = x as f32;
            (x, compressor_gain_db_curve(x, T, R, K, curve))
        })
        .collect()
}

#[test]
fn clean_matches_legacy_formula_byte_for_byte() {
    for x_db in [-40.0, -20.0, -14.0, -10.0, -6.0, 0.0] {
        let legacy = compressor_gain_db(x_db, T, R, K);
        let curved = compressor_gain_db_curve(x_db, T, R, K, CompressorCurve::Clean);
        assert_eq!(
            legacy.to_bits(),
            curved.to_bits(),
            "Clean curve must match compressor_gain_db at {x_db} dB"
        );
    }
}

#[test]
fn every_curve_is_monotonically_non_increasing_gr() {
    for &curve in &[CompressorCurve::Clean, CompressorCurve::Pump, CompressorCurve::Smooth] {
        let pts = sweep(curve);
        let mut prev = 0.0_f32;
        for &(x, gr) in &pts {
            assert!(
                gr <= 1e-3,
                "{curve:?}: curve produced positive gain at {x} dB ({gr})"
            );
            assert!(
                gr <= prev + 1e-3,
                "{curve:?}: GR rose with input at {x}: prev={prev}, gr={gr}"
            );
            prev = gr;
        }
    }
}

#[test]
fn pump_is_distinct_from_clean_just_past_threshold() {
    // Pump applies a +25% slope boost smoothly across the 6 dB just past
    // the right knee edge. At ~3 dB past the edge that should be a clear
    // half-dB-or-more difference vs Clean.
    let edge = T + K * 0.5;
    let test_db = edge + 3.0;
    let clean = compressor_gain_db_curve(test_db, T, R, K, CompressorCurve::Clean);
    let pump = compressor_gain_db_curve(test_db, T, R, K, CompressorCurve::Pump);
    println!("at {test_db} dB:  Clean = {clean:.3}  Pump = {pump:.3}");
    assert!(
        pump < clean - 0.3,
        "Pump should compress noticeably more than Clean ({pump} vs {clean})"
    );
    // Far past the pump region the boost has decayed — curves should
    // converge again to the same slope (parallel lines, but the curve
    // hits its full ratio at slightly different inputs so a tiny gap
    // remains; allow 1 dB).
    let far = edge + 30.0;
    let clean_far = compressor_gain_db_curve(far, T, R, K, CompressorCurve::Clean);
    let pump_far = compressor_gain_db_curve(far, T, R, K, CompressorCurve::Pump);
    assert!(
        (clean_far - pump_far).abs() < 1.0,
        "Pump should converge to Clean far from threshold (Δ={})",
        (clean_far - pump_far).abs()
    );
}

#[test]
fn smooth_is_distinct_from_clean_inside_the_knee() {
    // Smooth uses cubic smoothstep inside the knee; at the centre of the
    // knee the value is 0.5 of the right-edge cap, whereas Clean gives
    // (knee/2)² / (2 * knee) = knee/8. They differ by knee_half × (3·0.25 -
    // 2·0.125) - knee/8 = knee × (0.5 - 0.125) = 3·knee/8 on the slope,
    // times slope. Just check they're not equal at threshold itself.
    let clean = compressor_gain_db_curve(T, T, R, K, CompressorCurve::Clean);
    let smooth = compressor_gain_db_curve(T, T, R, K, CompressorCurve::Smooth);
    println!("at threshold {T} dB:  Clean = {clean:.4}  Smooth = {smooth:.4}");
    assert!(
        (clean - smooth).abs() > 0.05,
        "Smooth must differ from Clean inside the knee"
    );
    // Both must hit zero GR well below threshold.
    let below = compressor_gain_db_curve(T - K, T, R, K, CompressorCurve::Smooth);
    assert!(below.abs() < 1e-3, "Smooth must be 0 dB at -knee below threshold");
}

#[test]
fn all_three_curves_agree_far_above_threshold() {
    // Past the knee, all three should converge to the same hard-knee
    // line `slope * (input - threshold)`. Pump's boost has decayed by
    // 6 dB above; allow 1 dB tolerance for Pump's residual lag.
    let test_db = T + K * 0.5 + 20.0;
    let clean = compressor_gain_db_curve(test_db, T, R, K, CompressorCurve::Clean);
    let pump = compressor_gain_db_curve(test_db, T, R, K, CompressorCurve::Pump);
    let smooth = compressor_gain_db_curve(test_db, T, R, K, CompressorCurve::Smooth);
    println!("at {test_db} dB:  Clean = {clean:.2}  Pump = {pump:.2}  Smooth = {smooth:.2}");
    assert!((clean - smooth).abs() < 0.05, "Smooth must match Clean past knee");
    assert!((clean - pump).abs() < 1.0, "Pump must converge to Clean ({clean} vs {pump})");
}
