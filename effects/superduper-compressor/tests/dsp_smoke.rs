//! DSP smoke tests for SuperDuper Compressor — verify the static
//! compression curve and the envelope detector behave per spec.

use superduper_synth_core::dsp_blocks::{compressor_gain_db, EnvelopeDetector};

#[test]
fn below_threshold_no_compression() {
    // Anything ≥ knee/2 below threshold → 0 dB GR.
    let gr = compressor_gain_db(-30.0, -18.0, 4.0, 6.0);
    assert!(gr.abs() < 1e-3, "below knee shouldn't compress (gr={gr})");
}

#[test]
fn far_above_threshold_full_ratio() {
    // 20 dB above threshold, ratio 4:1, no knee → output rises 5 dB,
    // so GR = -15 dB.
    let gr = compressor_gain_db(0.0, -20.0, 4.0, 0.0);
    println!("hard knee, 20 dB over thr, 4:1 → gr = {gr:.2}");
    assert!((gr - (-15.0)).abs() < 0.1, "expected -15 dB, got {gr}");
}

#[test]
fn soft_knee_smooth_transition() {
    // Across the knee, gain reduction must be monotonic non-positive.
    let thr = -18.0;
    let knee = 8.0;
    let mut prev = 0.0_f32;
    for x_db in -30..0 {
        let gr = compressor_gain_db(x_db as f32, thr, 4.0, knee);
        assert!(gr <= 1e-3, "compressor produced positive gain change at {x_db}: {gr}");
        assert!(gr <= prev + 1e-3, "non-monotonic: prev={prev}, gr={gr} at {x_db}");
        prev = gr;
    }
}

#[test]
fn detector_attacks_and_releases() {
    // Hit detector with 1.0, verify it climbs; drop to 0, verify it falls.
    let sr = 48000.0;
    let mut d = EnvelopeDetector::default();
    let mut env_after_attack = 0.0;
    for _ in 0..(sr as usize / 100) {
        // 10 ms with input 1.0, 5 ms attack — should hit ~95 %.
        env_after_attack = d.process(1.0, sr, 5.0, 100.0);
    }
    let mut env_after_release = 0.0;
    for _ in 0..(sr as usize / 5) {
        // 200 ms silence, 100 ms release — should drop to ~12 %.
        env_after_release = d.process(0.0, sr, 5.0, 100.0);
    }
    println!("detector: attack→{env_after_attack:.3}, release→{env_after_release:.3}");
    assert!(env_after_attack > 0.85, "attack didn't reach 85% ({env_after_attack})");
    assert!(env_after_release < 0.20, "release didn't fall to 20% ({env_after_release})");
}

#[test]
fn ratio_one_no_compression() {
    // Ratio of exactly 1:1 = unity gain everywhere.
    for x_db in [-40, -20, -10, -5, 0] {
        let gr = compressor_gain_db(x_db as f32, -18.0, 1.0, 4.0);
        assert!(gr.abs() < 1e-3, "1:1 ratio should be unity, got {gr} at {x_db}");
    }
}
