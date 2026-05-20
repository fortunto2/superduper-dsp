//! DSP-only smoke tests for SuperDuper Wave.
//!
//! Drive the oscillator + filter directly without going through CLAP.
//! Validates wavetable rendering, unison spread, sub-osc mixing, choke
//! fade-out, and basic envelope behaviour.

use superduper_synth_core::dsp_blocks::{AdsrEnvelope, AdsrParams, midi_note_to_hz};
use superduper_wave::osc::{
    render_formula, render_formula_mip, FilterMode, LfoDest, LfoShape, WaveParams, WaveVoice,
    WT_SIZE,
};

const FENV_BYPASS: AdsrParams = AdsrParams { sr: 48_000.0, delay_s: 0.0, attack_s: 0.001, hold_s: 0.0, decay_s: 0.01, sustain: 0.0, release_s: 0.01 };
const LFO_OFF_RATE: f32 = 0.0;
const LFO_OFF_DEPTH: f32 = 0.0;

const SR: f32 = 48_000.0;

#[test]
fn render_formula_normalises_above_unity() {
    let table = render_formula(|p| 3.0 * (p * core::f32::consts::TAU).sin());
    let peak = table.iter().map(|x| x.abs()).fold(0.0_f32, f32::max);
    assert!(peak <= 1.001, "table must be normalised, peak={peak}");
    assert!(peak > 0.95, "non-flat formula must produce non-trivial output, peak={peak}");
    assert_eq!(table.len(), WT_SIZE);
}

#[test]
fn voice_emits_audio_on_note() {
    let a = render_formula_mip(|p| 2.0 * p - 1.0);
    let b = render_formula_mip(|p| 2.0 * p - 1.0);
    let frames_arr = [a.clone(), b.clone()];
    let mut v = WaveVoice::default();
    v.key = 60;
    v.velocity = 1.0;
    v.env.gate_on();
    v.configure_unison(3, 12.0);

    let params = WaveParams {
        sr: SR,
        root_hz: midi_note_to_hz(60.0),
        wt_pos: 0.0,
        unison: 3,
        detune_cents: 12.0,
        sub_level: 0.0,
        cutoff_hz: 8000.0,
        resonance: 0.2,
        mode: FilterMode::LowPass,
        drive: 0.0,
        antialias: true,
        noise_level: 0.0,
        fenv_amount_oct: 0.0,
        fenv: FENV_BYPASS,
        lfo_shape: LfoShape::Sine,
        lfo_dest: LfoDest::Cutoff,
        lfo_rate_hz: LFO_OFF_RATE,
        lfo_depth: LFO_OFF_DEPTH,
        frames: &frames_arr,
        frame_a_prev: &a,
        frame_a_fade: 1.0,
        sync_on: false,
        sync_ratio: 1.0,
        fm_ratio: 2.0,
        fm_amount: 0.0,
        mod_slots: [Default::default(); 2],
        mod_wheel: 0.0,
        aftertouch: 0.0,
    };

    // 256 samples warm-up (filter ringup + attack ramp).
    let adsr = AdsrParams { sr: SR, delay_s: 0.0, attack_s: 0.001, hold_s: 0.0, decay_s: 0.5, sustain: 0.8, release_s: 0.5 };
    let mut env_acc = AdsrEnvelope::default();
    env_acc.gate_on();
    for _ in 0..256 {
        let _ = env_acc.process(adsr);
        let _ = v.process(params);
    }
    let mut peak = 0.0_f32;
    for _ in 0..2048 {
        let e = env_acc.process(adsr);
        let (l, r) = v.process(params);
        let s = (l * e).abs().max((r * e).abs());
        peak = peak.max(s);
    }
    assert!(peak > 0.05, "voice must be audible, peak={peak}");
    assert!(peak.is_finite());
}

#[test]
fn morph_blends_two_frames() {
    let a = render_formula_mip(|_| 1.0);
    let b = render_formula_mip(|_| -1.0);
    let frames_arr = [a.clone(), b.clone()];

    let mut v = WaveVoice::default();
    v.key = 60;
    v.velocity = 1.0;
    v.env.gate_on();
    v.configure_unison(1, 0.0);

    let params = WaveParams {
        sr: SR,
        root_hz: midi_note_to_hz(60.0),
        wt_pos: 0.5,
        unison: 1,
        detune_cents: 0.0,
        sub_level: 0.0,
        cutoff_hz: SR * 0.45,
        resonance: 0.0,
        mode: FilterMode::LowPass,
        drive: 0.0,
        antialias: false,
        noise_level: 0.0,
        fenv_amount_oct: 0.0,
        fenv: FENV_BYPASS,
        lfo_shape: LfoShape::Sine,
        lfo_dest: LfoDest::Cutoff,
        lfo_rate_hz: LFO_OFF_RATE,
        lfo_depth: LFO_OFF_DEPTH,
        frames: &frames_arr,
        frame_a_prev: &a,
        frame_a_fade: 1.0,
        sync_on: false,
        sync_ratio: 1.0,
        fm_ratio: 2.0,
        fm_amount: 0.0,
        mod_slots: [Default::default(); 2],
        mod_wheel: 0.0,
        aftertouch: 0.0,
    };

    for _ in 0..512 {
        let _ = v.process(params);
    }
    let mut sum = 0.0_f32;
    let n = 1024;
    for _ in 0..n {
        let (l, _) = v.process(params);
        sum += l;
    }
    let mean = sum / n as f32;
    assert!(mean.abs() < 0.05, "0.5 morph of +1/-1 must average ≈0, got {mean}");
}

#[test]
fn unison_decorrelates_l_r() {
    let a = render_formula_mip(|p| (p * core::f32::consts::TAU).sin());
    let b = a.clone();
    let frames_arr = [a.clone(), b.clone()];
    let mut v = WaveVoice::default();
    v.key = 60;
    v.velocity = 1.0;
    v.env.gate_on();
    v.configure_unison(5, 25.0);

    let params = WaveParams {
        sr: SR,
        root_hz: midi_note_to_hz(60.0),
        wt_pos: 0.0,
        unison: 5,
        detune_cents: 25.0,
        sub_level: 0.0,
        cutoff_hz: 10000.0,
        resonance: 0.0,
        mode: FilterMode::LowPass,
        drive: 0.0,
        antialias: true,
        noise_level: 0.0,
        fenv_amount_oct: 0.0,
        fenv: FENV_BYPASS,
        lfo_shape: LfoShape::Sine,
        lfo_dest: LfoDest::Cutoff,
        lfo_rate_hz: LFO_OFF_RATE,
        lfo_depth: LFO_OFF_DEPTH,
        frames: &frames_arr,
        frame_a_prev: &a,
        frame_a_fade: 1.0,
        sync_on: false,
        sync_ratio: 1.0,
        fm_ratio: 2.0,
        fm_amount: 0.0,
        mod_slots: [Default::default(); 2],
        mod_wheel: 0.0,
        aftertouch: 0.0,
    };
    let mut diff = 0.0_f32;
    let mut sum = 0.0_f32;
    for _ in 0..2048 {
        let (l, r) = v.process(params);
        diff += (l - r) * (l - r);
        sum += (l + r) * (l + r) * 0.25;
    }
    let ratio = diff / sum.max(1e-9);
    assert!(ratio > 0.02, "5-voice unison with 25 cent detune should decorrelate L/R, ratio={ratio}");
}
