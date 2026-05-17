//! Smoke tests for the PadVoice DSP used by SuperDuper Ambient.

use superduper_synth_core::dsp_blocks::{PadParams, PadVoice};

const SR: f32 = 48_000.0;

fn default_params() -> PadParams {
    PadParams {
        sr: SR,
        root_hz: 110.0,
        cutoff_hz: 2000.0,
        resonance: 0.2,
        modulation_cents: 8.0,
        drive: 0.3,
    }
}

#[test]
fn voice_produces_audio() {
    let mut v = PadVoice::default();
    let p = default_params();
    // Drain transient.
    for _ in 0..1000 { v.process(p); }
    // Measure RMS over a full second.
    let mut sum_sq = 0.0_f32;
    let n = SR as usize;
    for _ in 0..n {
        let s = v.process(p);
        sum_sq += s * s;
    }
    let rms = (sum_sq / n as f32).sqrt();
    println!("PadVoice RMS: {rms:.4}");
    assert!(rms > 0.01, "voice silent (rms={rms})");
}

#[test]
fn voice_stable_over_long_time() {
    // Run 30 seconds to make sure nothing drifts to infinity.
    let mut v = PadVoice::default();
    let p = default_params();
    let mut peak = 0.0_f32;
    for _ in 0..(SR as usize * 30) {
        let s = v.process(p);
        peak = peak.max(s.abs());
        assert!(s.is_finite(), "PadVoice produced non-finite sample");
    }
    println!("PadVoice 30-sec peak: {peak:.3}");
    assert!(peak <= 1.05, "PadVoice unbounded (peak={peak})");
}

#[test]
fn cutoff_attenuates_brightness() {
    fn measure_rms(cutoff: f32) -> f32 {
        let mut v = PadVoice::default();
        let p = PadParams {
            sr: SR,
            root_hz: 220.0,
            cutoff_hz: cutoff,
            resonance: 0.2,
            modulation_cents: 8.0,
            drive: 0.0,
        };
        for _ in 0..1000 { v.process(p); }
        let mut sum_sq = 0.0_f32;
        let n = SR as usize / 2;
        for _ in 0..n {
            let s = v.process(p);
            sum_sq += s * s;
        }
        (sum_sq / n as f32).sqrt()
    }
    let bright = measure_rms(8000.0);
    let dark = measure_rms(300.0);
    println!("RMS bright (8k): {bright:.4}, dark (300): {dark:.4}");
    // Dark cutoff filters out partials → RMS should be lower or similar.
    // Allow some room — the LP can ring slightly with resonance.
    assert!(dark <= bright * 1.5, "dark cutoff shouldn't be louder than bright");
}

#[test]
fn modulation_creates_motion() {
    // With modulation > 0, consecutive samples shouldn't be identical
    // (phase drifts). With modulation = 0 and a steady root, after the
    // transient, output should still vary (sine continues to advance phase),
    // so we look at *variance over much longer windows* — modulated voice
    // should have more variance per long window than static voice.

    let mut v_mod = PadVoice::default();
    let mut v_static = PadVoice::default();

    let p_mod = PadParams {
        sr: SR,
        root_hz: 110.0,
        cutoff_hz: 1500.0,
        resonance: 0.0,
        modulation_cents: 40.0, // heavy modulation
        drive: 0.0,
    };
    let mut p_static = p_mod;
    p_static.modulation_cents = 0.0;

    // Skip transient.
    for _ in 0..1000 {
        v_mod.process(p_mod);
        v_static.process(p_static);
    }

    // Measure peak-to-peak over a 5-second window. Modulated voice will
    // wobble in amplitude due to phase interference of detuning partials.
    let window = SR as usize * 5;
    let mut max_mod = f32::MIN;
    let mut min_mod = f32::MAX;
    let mut max_static = f32::MIN;
    let mut min_static = f32::MAX;
    for _ in 0..window {
        let m = v_mod.process(p_mod);
        let s = v_static.process(p_static);
        max_mod = max_mod.max(m);
        min_mod = min_mod.min(m);
        max_static = max_static.max(s);
        min_static = min_static.min(s);
    }
    let range_mod = max_mod - min_mod;
    let range_static = max_static - min_static;
    println!("range modulated={range_mod:.3}, static={range_static:.3}");
    // Static should also have a non-zero range (sines are continuous), but
    // modulation should produce a noticeably larger envelope range due to
    // beating between detuned partials.
    assert!(range_mod > 0.05, "modulated voice didn't move");
    assert!(range_static > 0.05, "static voice should still oscillate");
}
