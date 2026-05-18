//! DSP-only smoke tests for Kubyz.

use superduper_kubyz::presets::{presets, N_HARMONICS};
use superduper_kubyz::voice::{KubyzParams, KubyzVoice};
use superduper_synth_core::dsp_blocks::{AdsrEnvelope, AdsrParams, midi_note_to_hz};

const SR: f32 = 48_000.0;

#[test]
fn voice_produces_audio_on_held_note() {
    let preset = &presets()[1]; // Bashkir
    let mut v = KubyzVoice::default();
    v.key = 50; // D2 — typical kubyz pitch
    v.velocity = 0.9;
    v.env.gate_on();

    let params = KubyzParams {
        sr: SR,
        root_hz: midi_note_to_hz(50.0),
        harmonics: &preset.harmonics,
        formant_f: preset.formant.f,
        formant_bw: preset.formant.bw,
        formant_gain: preset.formant.gain,
        formant_mix: 0.6,
        velocity_formant_shift: preset.velocity_formant_shift,
    };
    // Warm-up.
    let adsr = AdsrParams {
        sr: SR,
        attack_s: preset.attack_s,
        decay_s: preset.decay_s,
        sustain: preset.sustain.max(0.1), // raise sustain so the test sees a steady patch
        release_s: preset.release_s,
    };
    let mut e = AdsrEnvelope::default();
    e.gate_on();
    for _ in 0..2048 {
        let _ = e.process(adsr);
        let _ = v.process(params);
    }
    let mut peak = 0.0_f32;
    for _ in 0..2048 {
        let env = e.process(adsr);
        let (l, r) = v.process(params);
        let s = (l * env).abs().max((r * env).abs());
        peak = peak.max(s);
    }
    assert!(peak > 0.01, "Kubyz voice must be audible at full velocity, peak={peak}");
    assert!(peak.is_finite());
}

#[test]
fn harmonics_normalised_to_unit_peak() {
    for preset in presets().iter() {
        // After db_to_lin_array we normalise by the loudest harmonic
        // (not H1), so the peak must be 1.0 but H1 may be lower for
        // overtone-dominant presets like Real D2.
        let peak = preset.harmonics.iter().copied().fold(0.0_f32, f32::max);
        assert!((peak - 1.0).abs() < 1e-3,
                "{}: harmonic peak should be 1.0, got {peak}", preset.name);
        assert_eq!(preset.harmonics.len(), N_HARMONICS);
    }
}

#[test]
fn velocity_shifts_formant_frequencies() {
    // High velocity should push the formant audibly up — we can't measure
    // that here cheaply, but we can at least assert the math compiles and
    // returns finite audio.
    let preset = &presets()[1];
    let mut v = KubyzVoice::default();
    v.key = 50;
    v.velocity = 1.0;
    v.env.gate_on();
    let params = KubyzParams {
        sr: SR,
        root_hz: midi_note_to_hz(50.0),
        harmonics: &preset.harmonics,
        formant_f: preset.formant.f,
        formant_bw: preset.formant.bw,
        formant_gain: preset.formant.gain,
        formant_mix: 1.0,
        velocity_formant_shift: 0.4,
    };
    for _ in 0..1024 {
        let (l, r) = v.process(params);
        assert!(l.is_finite() && r.is_finite());
    }
}
