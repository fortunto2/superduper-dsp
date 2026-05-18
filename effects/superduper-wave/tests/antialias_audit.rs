//! antialias_audit.rs — measure spectral aliasing of the wavetable
//! oscillator with anti-alias OFF vs ON, on a saw-waveform high note.
//! Proves the mip-map pyramid actually cleans HF folding-back.
//!
//! Strategy: play MIDI 96 (C7 ≈ 2093 Hz) through a saw preset (frame_a =
//! frame_b = ideal sawtooth, all harmonics), filter wide-open, drive 0.
//! Then FFT the steady-state output and ask `measure_aliasing_db` for
//! the worst non-harmonic peak relative to the fundamental.

use superduper_synth_core::analysis::measure_aliasing_db;
use superduper_synth_core::dsp_blocks::{AdsrEnvelope, AdsrParams, midi_note_to_hz};
use superduper_wave::osc::{
    render_formula_mip, FilterMode, LfoDest, LfoShape, WaveParams, WaveVoice,
};

const SR: f32 = 48_000.0;
const FFT_LEN: usize = 8192;

fn run_voice(antialias: bool) -> f32 {
    // Ideal sawtooth — every harmonic above Nyquist will fold without
    // mip-mapping.
    let a = render_formula_mip(|p| 2.0 * p - 1.0);
    let b = a.clone();

    let mut v = WaveVoice::default();
    let midi_note = 96.0_f32; // C7 — very high; loads of harmonics above Nyquist.
    v.key = midi_note as u8;
    v.velocity = 1.0;
    v.env.gate_on();
    v.configure_unison(1, 0.0);

    let params = WaveParams {
        sr: SR,
        root_hz: midi_note_to_hz(midi_note),
        wt_pos: 0.0,
        unison: 1,
        detune_cents: 0.0,
        sub_level: 0.0,
        cutoff_hz: SR * 0.45, // filter wide-open so it can't hide aliasing
        resonance: 0.0,
        mode: FilterMode::LowPass,
        drive: 0.0,
        antialias,
        noise_level: 0.0,
        fenv_amount_oct: 0.0,
        fenv: AdsrParams { sr: SR, delay_s: 0.0, attack_s: 0.001, hold_s: 0.0, decay_s: 0.01, sustain: 0.0, release_s: 0.01 },
        lfo_shape: LfoShape::Sine,
        lfo_dest: LfoDest::Cutoff,
        lfo_rate_hz: 0.0,
        lfo_depth: 0.0,
        frame_a: &a,
        frame_a_prev: &a,
        frame_a_fade: 1.0,
        frame_b: &b,
        mod_slots: [Default::default(); 2],
        mod_wheel: 0.0,
        aftertouch: 0.0,
    };

    // Warm-up: let filter ring out, envelope reach unity.
    let adsr = AdsrParams { sr: SR, delay_s: 0.0, attack_s: 0.001, hold_s: 0.0, decay_s: 1.0, sustain: 1.0, release_s: 1.0 };
    let mut env = AdsrEnvelope::default();
    env.gate_on();
    for _ in 0..2048 {
        let _ = env.process(adsr);
        let _ = v.process(params);
    }

    // Capture FFT_LEN steady-state samples.
    let mut samples = Vec::with_capacity(FFT_LEN);
    for _ in 0..FFT_LEN {
        let _ = env.process(adsr);
        let (l, r) = v.process(params);
        samples.push(0.5 * (l + r));
    }
    measure_aliasing_db(&samples, midi_note_to_hz(midi_note), SR)
}

#[test]
fn mip_map_reduces_aliasing_on_high_saw_notes() {
    let off = run_voice(false);
    let on = run_voice(true);
    eprintln!("Saw @ C7 — aliasing relative to fundamental:");
    eprintln!("  Anti-Alias OFF: {off:.1} dB");
    eprintln!("  Anti-Alias ON : {on:.1} dB");
    eprintln!("  improvement   : {:.1} dB", off - on);
    assert!(
        off > -40.0,
        "without mip-map a saw at C7 should fold loud aliasing back into band; \
         measured {off:.1} dB — analyser is broken or preset wrong"
    );
    assert!(
        on < off - 12.0,
        "anti-alias should cut at least 12 dB off the alias floor; \
         got OFF={off:.1} dB, ON={on:.1} dB (improvement {:.1} dB)",
        off - on
    );
}
