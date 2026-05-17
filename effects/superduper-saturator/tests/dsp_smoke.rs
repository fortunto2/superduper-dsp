//! DSP smoke tests for SuperDuper Saturator. Direct calls to the shared
//! saturation curves — confirms each shape actually limits, doesn't blow
//! up, and produces a different result from the input.

use superduper_synth_core::dsp_blocks::{tanh_drive, tape_clip, tube_clip};

fn rms(samples: &[f32]) -> f32 {
    let n = samples.len() as f32;
    let s: f32 = samples.iter().map(|x| x * x).sum();
    (s / n).sqrt()
}

#[test]
fn tanh_bounded() {
    let mut peak = 0.0_f32;
    for i in 0..1000 {
        let x = (i as f32 - 500.0) / 100.0; // -5..5
        let y = tanh_drive(x, 4.0);
        peak = peak.max(y.abs());
    }
    assert!(peak <= 1.0, "tanh output exceeded ±1: {peak}");
}

#[test]
fn tape_bounded() {
    let mut peak = 0.0_f32;
    for i in 0..1000 {
        let x = (i as f32 - 500.0) / 50.0; // -10..10
        let y = tape_clip(x, 4.0);
        peak = peak.max(y.abs());
    }
    assert!(peak <= 1.0, "tape output exceeded ±1: {peak}");
}

#[test]
fn tube_bounded_and_asymmetric() {
    // Drive a sine through tube curve, verify it's bounded and that
    // the positive/negative halves DIFFER (that's the whole point).
    let sr = 48000.0_f32;
    let mut pos_rms = 0.0;
    let mut neg_rms = 0.0;
    let mut pos_n = 0.0_f32;
    let mut neg_n = 0.0_f32;
    let mut peak = 0.0_f32;
    for i in 0..2048 {
        let phase = i as f32 * 2.0 * core::f32::consts::PI * 100.0 / sr;
        let x = phase.sin() * 0.7;
        let y = tube_clip(x, 3.0);
        peak = peak.max(y.abs());
        if y >= 0.0 {
            pos_rms += y * y;
            pos_n += 1.0;
        } else {
            neg_rms += y * y;
            neg_n += 1.0;
        }
    }
    let pos = (pos_rms / pos_n).sqrt();
    let neg = (neg_rms / neg_n).sqrt();
    println!("tube: pos_rms={pos:.4} neg_rms={neg:.4} peak={peak:.4}");
    assert!(peak < 2.0, "tube unbounded (peak={peak})");
    assert!(
        (pos - neg).abs() > 0.01,
        "tube curve is symmetric (pos={pos}, neg={neg}); should differ for class-A character"
    );
}

#[test]
fn drive_increases_harmonics() {
    // A pure sine through tape with low drive should look mostly like a
    // sine; at high drive it gets squashed → RMS rises relative to peak.
    let sr = 48000.0_f32;
    let make = |drive: f32| -> Vec<f32> {
        (0..4096)
            .map(|i| {
                let p = i as f32 * 2.0 * core::f32::consts::PI * 220.0 / sr;
                tape_clip(p.sin() * 0.6, drive)
            })
            .collect()
    };
    let soft = make(1.0);
    let hot = make(16.0);
    let soft_rms = rms(&soft);
    let hot_rms = rms(&hot);
    println!("tape drive 1→16: rms {soft_rms:.4} → {hot_rms:.4}");
    assert!(hot_rms > soft_rms, "higher drive should not reduce RMS");
}
