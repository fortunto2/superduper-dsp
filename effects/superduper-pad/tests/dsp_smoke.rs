//! DSP-only smoke tests for the Pad synth. Drives the voice pool directly
//! without going through CLAP plumbing — fast loop for iterating on the
//! envelope curve and voice stealer.

use superduper_synth_core::dsp_blocks::{
    AdsrEnvelope, AdsrParams, AdsrStage, PadParams, PadVoice, midi_note_to_hz,
};

const SR: f32 = 48000.0;

/// MIDI key → Hz must match the standard 12-TET formula with A4=440.
#[test]
fn midi_note_table_matches_standard_pitches() {
    assert!((midi_note_to_hz(69.0) - 440.0).abs() < 1e-3, "A4 must be 440");
    assert!((midi_note_to_hz(60.0) - 261.6256).abs() < 0.01, "C4 ~ 261.63");
    assert!((midi_note_to_hz(81.0) - 880.0).abs() < 1e-2, "A5 must be 880");
    // 12-semitone interval doubles frequency.
    assert!((midi_note_to_hz(72.0) / midi_note_to_hz(60.0) - 2.0).abs() < 1e-4);
}

/// ADSR with very short attack should ramp to ~1.0 within a few ms.
#[test]
fn adsr_attack_reaches_unity() {
    let mut env = AdsrEnvelope::default();
    env.gate_on();
    let p = AdsrParams { sr: SR, attack_s: 0.001, decay_s: 1.0, sustain: 0.7, release_s: 1.0 };
    // 1 ms = 48 samples. Should reach >0.99 within twice that.
    let mut peak = 0.0_f32;
    for _ in 0..100 {
        peak = peak.max(env.process(p));
    }
    assert!(peak > 0.99, "attack should reach unity, got {peak}");
}

/// ADSR with a finite sustain should glide there after attack/decay.
#[test]
fn adsr_decays_to_sustain() {
    let mut env = AdsrEnvelope::default();
    env.gate_on();
    let p = AdsrParams { sr: SR, attack_s: 0.001, decay_s: 0.01, sustain: 0.4, release_s: 1.0 };
    // Run long enough to reach sustain (~1 s safely covers attack + decay).
    for _ in 0..(SR as usize) {
        env.process(p);
    }
    let lvl = env.level();
    assert!(env.stage() == AdsrStage::Sustain, "should be in sustain, got {:?}", env.stage());
    assert!((lvl - 0.4).abs() < 0.02, "sustain level off: {lvl}");
}

/// gate_off triggers a release that monotonically falls toward zero.
#[test]
fn adsr_release_falls_to_zero() {
    let mut env = AdsrEnvelope::default();
    env.gate_on();
    let p = AdsrParams { sr: SR, attack_s: 0.001, decay_s: 0.01, sustain: 0.6, release_s: 0.05 };
    // Reach sustain.
    for _ in 0..(SR as usize / 10) {
        env.process(p);
    }
    env.gate_off();
    let mut prev = env.level();
    let mut went_idle = false;
    for _ in 0..(SR as usize) {
        let lvl = env.process(p);
        // Strict monotone release (allow numerical noise).
        assert!(lvl <= prev + 1e-6, "release should be monotone, lvl={lvl} prev={prev}");
        prev = lvl;
        if env.is_idle() {
            went_idle = true;
            break;
        }
    }
    assert!(went_idle, "envelope should reach idle within 1 s after release");
}

/// A held note must produce non-trivial output via PadVoice.
#[test]
fn pad_voice_produces_signal_at_midi_60() {
    let mut voice = PadVoice::default();
    let hz = midi_note_to_hz(60.0);
    let p = PadParams {
        sr: SR,
        root_hz: hz,
        cutoff_hz: 3500.0,
        resonance: 0.2,
        modulation_cents: 0.0,
        drive: 0.2,
    };
    let mut peak = 0.0_f32;
    let mut energy = 0.0_f32;
    let n = 2048;
    // Skip filter ringup transient before measuring.
    for _ in 0..512 { let _ = voice.process(p); }
    for _ in 0..n {
        let s = voice.process(p);
        peak = peak.max(s.abs());
        energy += s * s;
    }
    let rms = (energy / n as f32).sqrt();
    assert!(peak > 0.05, "Pad voice should be audible, peak={peak}");
    assert!(rms > 0.01, "RMS too low: {rms}");
    assert!(peak.is_finite() && rms.is_finite(), "output must be finite");
}

/// Stereo width — two voices detuned by ±width/2 cents on the same root
/// must produce distinct signals (the whole point of stereo width).
#[test]
fn pad_stereo_width_creates_decorrelation() {
    let mut l = PadVoice::default();
    let mut r = PadVoice::default();
    let base_hz = midi_note_to_hz(60.0);
    let width_cents = 14.0_f32;
    let l_hz = base_hz * 2f32.powf(-width_cents * 0.5 / 1200.0);
    let r_hz = base_hz * 2f32.powf(width_cents * 0.5 / 1200.0);
    let mut diff_energy = 0.0_f32;
    let mut sum_energy = 0.0_f32;
    let pl = PadParams { sr: SR, root_hz: l_hz, cutoff_hz: 3500.0, resonance: 0.2, modulation_cents: 0.0, drive: 0.0 };
    let pr = PadParams { root_hz: r_hz, ..pl };
    // Long enough to let detune-phase difference accumulate (LFOs are slow).
    for _ in 0..(SR as usize) {
        let a = l.process(pl);
        let b = r.process(pr);
        diff_energy += (a - b) * (a - b);
        sum_energy += (a + b) * (a + b) * 0.25;
    }
    let ratio = diff_energy / sum_energy.max(1e-9);
    assert!(ratio > 0.05, "detune should decorrelate L/R noticeably, ratio={ratio}");
}
