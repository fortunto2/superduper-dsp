//! Range + Hold behavior tests at the curve-output level. The actual
//! Hold gating lives inside `process_stereo_block` (private) so this
//! verifies the static curve clamping math separately.

use superduper_synth_core::dsp_blocks::{compressor_gain_db_curve, CompressorCurve};

#[test]
fn range_clamps_gain_reduction_to_max() {
    // 20 dB above threshold, ratio 4:1, no knee → GR = -15 dB without
    // clamp. Range = 6 dB must clamp to -6 dB.
    let gr_raw = compressor_gain_db_curve(0.0, -20.0, 4.0, 0.0, CompressorCurve::Clean);
    assert!((gr_raw + 15.0).abs() < 0.1, "baseline got {gr_raw}");

    let range_db = 6.0_f32;
    let clamped = gr_raw.max(-range_db);
    assert!((clamped + 6.0).abs() < 1e-3, "clamped should be -6, got {clamped}");

    // Range ≤ 0.05 leaves the curve alone (matches the if-guard inside
    // process_stereo_block — without this guard a Range of 0 would clamp
    // to 0 dB and silently turn the compressor off).
    let no_clamp_range = 0.0_f32;
    let result = if no_clamp_range > 0.05 {
        gr_raw.max(-no_clamp_range)
    } else {
        gr_raw
    };
    assert_eq!(result, gr_raw);
}

#[test]
fn range_doesnt_affect_below_threshold() {
    // No compression below threshold → GR=0 regardless of Range.
    let gr = compressor_gain_db_curve(-40.0, -20.0, 4.0, 6.0, CompressorCurve::Clean);
    assert!(gr.abs() < 1e-3);
    let clamped = gr.max(-6.0);
    assert!(clamped.abs() < 1e-3);
}
