//! C ABI for an in-app SuperDuper synth on iOS. Driven by note_on/note_off/set_param (the same
//! events the live2play gesture engine already emits as MIDI), rendered per audio block by
//! `sdsp_process` from a Swift `AVAudioSourceNode`.
//!
//! Spike scope: a polyphonic SuperDuper *Ambient* pad (the real `PadVoice` DSP from synth-core) with
//! a simple per-voice AR envelope. The next layer replaces the engine internals with a clack-host
//! plugin chain — the C ABI below stays the same.
//!
//! THREADING (spike): control fns (note/param) are called from the main thread, `sdsp_process` from
//! the audio thread. The shared state is plain POD (no Vec resize after create), so a torn read is
//! at worst a momentary glitch, never a crash. The production version routes control through an SPSC
//! command ring (like livehub's rtrb) for correctness.

use fundsp::prelude::{AudioUnit, Net};
use superduper_synth_core::dsp_blocks::{
    midi_note_to_hz, tanh_drive, AdsrParams, DelayLine, OnePoleLp, PadParams, PadVoice,
};
use superduper_synth_core::drum_voices::{
    note_to_voice, Clap, Cowbell, DrumParams, HiHat, Kick, Snare, VoiceKind,
};
use superduper_synth_core::kubyz::voice::{KubyzParams, KubyzVoice};
use superduper_synth_core::kubyz::N_HARMONICS;
use superduper_synth_core::supermass;
use superduper_synth_core::wave_osc::{
    render_formula_mip, FilterMode, LfoDest, LfoShape, MipWavetable, ModSlot, WaveParams, WaveVoice,
};

const MAX_VOICES: usize = 16;

/// Instrument engines the chain can run (picked from a list in the Synth panel).
const INSTR_AMBIENT: u32 = 0; // PadVoice drone pad
const INSTR_WAVE: u32 = 1; // wavetable synth (superduper-wave DSP, extracted to synth-core)
const INSTR_KUBYZ: u32 = 2; // Bashkir jaw-harp additive model (superduper-kubyz DSP)
const INSTR_DRUM: u32 = 3; // 6 analog drum voices (superduper-drum DSP) — notes map to drums

// Built-in Kubyz timbre (Bashkir reference): per-harmonic levels in dB + a fixed formant. Converted
// to linear at create. The plugin exposes these as editable; iOS ships the reference voice.
const KUBYZ_HARM_DB: [f32; N_HARMONICS] = [
    0.0, 6.6, 19.0, 24.1, 17.0, 38.6, 9.4, 16.7, 15.2, 17.9, 19.9, 9.8, 14.3, -0.5, 7.6, 3.3,
];
const KUBYZ_FORMANT_F: [f32; 3] = [705.0, 1301.0, 2165.0];
const KUBYZ_FORMANT_BW: [f32; 3] = [90.0, 110.0, 130.0];
const KUBYZ_FORMANT_GAIN: [f32; 3] = [1.0, 0.8, 0.6];

// Built-in wavetable frames (phase 0..1 → amplitude). render_formula_mip band-limits each via FFT,
// so these naive shapes are anti-aliased per mip level. WT Pos morphs between adjacent frames.
fn wt_saw(p: f32) -> f32 { 2.0 * p - 1.0 }
fn wt_square(p: f32) -> f32 { if p < 0.5 { 1.0 } else { -1.0 } }
fn wt_triangle(p: f32) -> f32 { 4.0 * (p - 0.5).abs() - 1.0 }
fn wt_pulse(p: f32) -> f32 { if p < 0.25 { 1.0 } else { -1.0 } }

/// One FX slot of the chain — a SuperDuper effect the user picks from a list (the `track_fx`
/// equivalent on iOS). Built once at create so switching is just an id change (RT-safe, no alloc).
/// 0 Off · 1 Reverb (supermass) · 2 Filter (one-pole LP sweep) · 3 Saturator (tanh drive).
const FX_OFF: u32 = 0;
const FX_REVERB: u32 = 1;
const FX_FILTER: u32 = 2;
const FX_SATURATE: u32 = 3;
const FX_DELAY: u32 = 4; // feedback delay (DelayLine)
const FX_CHORUS: u32 = 5; // LFO-modulated short delay

struct Voice {
    l: PadVoice,
    r: PadVoice,
    wave: WaveVoice,   // Wave instrument (osc/filter state)
    kubyz: KubyzVoice, // Kubyz instrument (additive + formant state)
    key: u8,
    gate: bool, // note currently held
    env: f32,   // 0..1 AR envelope, drives amplitude for ALL instruments
}

impl Voice {
    fn new() -> Self {
        Self {
            l: PadVoice::default(), r: PadVoice::default(),
            wave: WaveVoice::default(), kubyz: KubyzVoice::default(),
            key: 255, gate: false, env: 0.0,
        }
    }
}

pub struct Engine {
    sr: f32,
    voices: Vec<Voice>,
    // Each param keeps a TARGET (set by control) + a smoothed CURRENT (used per sample) so knob /
    // gesture moves glide instead of stepping (no zipper) — real-time playable.
    cutoff: f32, cutoff_t: f32,     // Hz
    resonance: f32, resonance_t: f32, // 0..0.95
    drive: f32, drive_t: f32,        // 0..1
    mod_cents: f32, mod_cents_t: f32, // Ambient: LFO detune depth
    wt_pos: f32, wt_pos_t: f32,       // Wave: wavetable morph position 0..1 (param 3, raw)
    // Instrument selector + Wave wavetable frames (built once at create — FFT mip pyramid).
    instrument: u32,
    wave_frames: Vec<MipWavetable>,
    wave_prev: MipWavetable,
    kubyz_harm: [f32; N_HARMONICS], // Kubyz harmonic levels (linear), built from the dB reference
    // Drum kit (fixed voices, triggered by note — not the poly pool).
    kick: Kick, snare: Snare, hat: HiHat, clap: Clap, cowbell: Cowbell,
    hat_decay: f32, // open vs closed hat
    // FX chain slot (post-instrument). Pre-built; `fx` selects which runs.
    fx: u32,
    fx_amt: f32, fx_amt_t: f32, // wet / amount, smoothed
    reverb: Net,
    filt_l: OnePoleLp, filt_r: OnePoleLp,
    delay_l: DelayLine, delay_r: DelayLine, // shared by Delay + Chorus
    chorus_phase: f32,
}

#[no_mangle]
pub extern "C" fn sdsp_create(sample_rate: f32) -> *mut Engine {
    let sr = if sample_rate > 0.0 { sample_rate } else { 48_000.0 };
    let mut reverb = supermass::build_wet();
    reverb.set_sample_rate(sr as f64); // RT-unsafe build/config done here, once — tick() in process is RT-safe
    // Wave wavetable frames (FFT mip pyramids — build here, never in process).
    let wave_frames: Vec<MipWavetable> =
        vec![render_formula_mip(wt_saw), render_formula_mip(wt_square),
             render_formula_mip(wt_triangle), render_formula_mip(wt_pulse)];
    let wave_prev = render_formula_mip(wt_saw);
    let mut kubyz_harm = [0.0_f32; N_HARMONICS]; // dB → linear once
    for (i, db) in KUBYZ_HARM_DB.iter().enumerate() { kubyz_harm[i] = 10.0_f32.powf(db / 20.0); }
    let e = Box::new(Engine {
        sr,
        voices: (0..MAX_VOICES).map(|_| Voice::new()).collect(),
        cutoff: 2_400.0, cutoff_t: 2_400.0,
        resonance: 0.30, resonance_t: 0.30,
        drive: 0.25, drive_t: 0.25,
        mod_cents: 25.0, mod_cents_t: 25.0,
        wt_pos: 0.0, wt_pos_t: 0.0,
        instrument: INSTR_AMBIENT,
        wave_frames, wave_prev, kubyz_harm,
        kick: Kick::default(), snare: Snare::default(), hat: HiHat::default(),
        clap: Clap::default(), cowbell: Cowbell::default(), hat_decay: 0.06,
        fx: FX_OFF,
        fx_amt: 0.35, fx_amt_t: 0.35,
        reverb,
        filt_l: OnePoleLp::default(), filt_r: OnePoleLp::default(),
        delay_l: DelayLine::new(sr as usize), delay_r: DelayLine::new(sr as usize), // 1 s
        chorus_phase: 0.0,
    });
    Box::into_raw(e)
}

/// Pick the instrument engine (0 Ambient pad · 1 Wave wavetable). Main thread.
#[no_mangle]
pub extern "C" fn sdsp_set_instrument(p: *mut Engine, id: u32) {
    if let Some(e) = unsafe { p.as_mut() } { e.instrument = id; }
}

/// Pick the FX-slot effect (0 off · 1 reverb · 2 filter · 3 saturator). Main thread.
#[no_mangle]
pub extern "C" fn sdsp_set_effect(p: *mut Engine, id: u32) {
    if let Some(e) = unsafe { p.as_mut() } { e.fx = id; }
}

/// FX wet/amount 0..1. Main thread; smoothed on the audio thread.
#[no_mangle]
pub extern "C" fn sdsp_set_effect_amount(p: *mut Engine, value: f32) {
    if let Some(e) = unsafe { p.as_mut() } { e.fx_amt_t = value.clamp(0.0, 1.0); }
}

#[no_mangle]
pub extern "C" fn sdsp_destroy(p: *mut Engine) {
    if !p.is_null() {
        unsafe { drop(Box::from_raw(p)) };
    }
}

#[no_mangle]
pub extern "C" fn sdsp_note_on(p: *mut Engine, key: u8, _velocity: f32) {
    let e = match unsafe { p.as_mut() } { Some(e) => e, None => return };
    if e.instrument == INSTR_DRUM {
        // Drums are one-shot voices triggered by note (C=kick, D=snare, E/F=hats, G=clap, A=cowbell).
        if let Some(kind) = note_to_voice(key) {
            match kind {
                VoiceKind::Kick => e.kick.trigger(1.0),
                VoiceKind::Snare => e.snare.trigger(1.0),
                VoiceKind::HatClosed => { e.hat_decay = 0.06; e.hat.trigger(1.0); }
                VoiceKind::HatOpen => { e.hat_decay = 0.40; e.hat.trigger(1.0); }
                VoiceKind::Clap => e.clap.trigger(1.0),
                VoiceKind::Cowbell => e.cowbell.trigger(1.0),
            }
        }
        return;
    }
    // Reuse a held voice on the same key, else the most-released (quietest) slot.
    let idx = e.voices.iter().position(|v| v.gate && v.key == key)
        .or_else(|| e.voices.iter().position(|v| !v.gate && v.env < 0.01))
        .unwrap_or_else(|| {
            e.voices.iter().enumerate()
                .min_by(|a, b| a.1.env.partial_cmp(&b.1.env).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(i, _)| i).unwrap_or(0)
        });
    let sr = e.sr;
    let v = &mut e.voices[idx];
    if v.env < 0.01 {
        // fresh slot → reset oscillator phase so the attack doesn't click
        v.l = PadVoice::default(); v.r = PadVoice::default(); v.wave = WaveVoice::default();
        v.kubyz = KubyzVoice::default();
    }
    v.kubyz.on_note_on(sr); // re-trigger the jaw-harp on-note fade
    v.key = key;
    v.gate = true;
}

#[no_mangle]
pub extern "C" fn sdsp_note_off(p: *mut Engine, key: u8) {
    let e = match unsafe { p.as_mut() } { Some(e) => e, None => return };
    for v in e.voices.iter_mut() {
        if v.gate && v.key == key { v.gate = false; }
    }
}

#[no_mangle]
pub extern "C" fn sdsp_all_notes_off(p: *mut Engine) {
    let e = match unsafe { p.as_mut() } { Some(e) => e, None => return };
    for v in e.voices.iter_mut() { v.gate = false; }
}

/// Map a 0..1 control value to a synth parameter. id: 0 cutoff · 1 resonance · 2 drive · 3 mod.
#[no_mangle]
pub extern "C" fn sdsp_set_param(p: *mut Engine, id: u32, value: f32) {
    let e = match unsafe { p.as_mut() } { Some(e) => e, None => return };
    let v = value.clamp(0.0, 1.0);
    match id {
        0 => e.cutoff_t = 150.0 + v * v * 9_000.0, // perceptual-ish cutoff sweep
        1 => e.resonance_t = v * 0.95,
        2 => e.drive_t = v,
        3 => { e.mod_cents_t = v * 60.0; e.wt_pos_t = v; } // Ambient: LFO detune · Wave: WT morph
        _ => {}
    }
}

/// Render `frames` stereo samples into the two output buffers. Audio-thread only; no allocation.
#[no_mangle]
pub extern "C" fn sdsp_process(p: *mut Engine, out_l: *mut f32, out_r: *mut f32, frames: u32) {
    let e = match unsafe { p.as_mut() } { Some(e) => e, None => return };
    let n = frames as usize;
    if out_l.is_null() || out_r.is_null() { return; }
    let ol = unsafe { core::slice::from_raw_parts_mut(out_l, n) };
    let or = unsafe { core::slice::from_raw_parts_mut(out_r, n) };
    // One-pole AR coefficients (~12 ms attack, ~700 ms release) + param smoothing (~20 ms) so knob /
    // gesture moves glide in real time without zipper noise.
    let atk = 1.0 - (-1.0 / (0.012 * e.sr)).exp();
    let rel = 1.0 - (-1.0 / (0.700 * e.sr)).exp();
    let psm = 1.0 - (-1.0 / (0.020 * e.sr)).exp();
    for i in 0..n {
        // Smooth params toward their targets, then snapshot for this sample (before borrowing voices).
        e.cutoff += (e.cutoff_t - e.cutoff) * psm;
        e.resonance += (e.resonance_t - e.resonance) * psm;
        e.drive += (e.drive_t - e.drive) * psm;
        e.mod_cents += (e.mod_cents_t - e.mod_cents) * psm;
        e.wt_pos += (e.wt_pos_t - e.wt_pos) * psm;
        e.fx_amt += (e.fx_amt_t - e.fx_amt) * psm;
        let (cutoff, resonance, drive, mod_cents, wt_pos) =
            (e.cutoff, e.resonance, e.drive, e.mod_cents, e.wt_pos);
        let (fx, amt, srate, instrument) = (e.fx, e.fx_amt, e.sr, e.instrument);
        let mut sl = 0.0_f32;
        let mut sr = 0.0_f32;
        if instrument == INSTR_DRUM {
            // Fixed drum kit (not the poly pool) — sum the 6 voices.
            let dp = DrumParams::default();
            let m = e.kick.process(srate, dp)
                + e.snare.process(srate, dp)
                + e.hat.process(srate, DrumParams { decay_s: e.hat_decay, ..dp })
                + e.clap.process(srate, dp)
                + e.cowbell.process(srate, dp);
            sl = m * 0.6;
            sr = m * 0.6;
        } else {
        for v in e.voices.iter_mut() {
            let target = if v.gate { 1.0 } else { 0.0 };
            let rate = if v.gate { atk } else { rel };
            v.env += (target - v.env) * rate;
            if !v.gate && v.env < 0.0005 { continue; }
            let f = midi_note_to_hz(v.key as f32);
            if instrument == INSTR_WAVE {
                // Wave wavetable voice — superduper-wave DSP (extracted to synth-core). It returns the
                // raw osc+filter pair; our shared AR `env` is the amplitude (its internal env unused).
                let params = WaveParams {
                    sr: srate, root_hz: f, wt_pos, unison: 1, detune_cents: 0.0,
                    sub_level: 0.0, noise_level: 0.0, cutoff_hz: cutoff, resonance,
                    mode: FilterMode::from_index(0), drive, antialias: true,
                    fenv_amount_oct: 0.0, fenv: AdsrParams::adsr(srate, 0.005, 0.1, 1.0, 0.1),
                    lfo_shape: LfoShape::from_index(0), lfo_dest: LfoDest::from_index(0),
                    lfo_rate_hz: 1.0, lfo_depth: 0.0,
                    frames: &e.wave_frames, frame_a_prev: &e.wave_prev, frame_a_fade: 1.0,
                    sync_on: false, sync_ratio: 1.0, fm_ratio: 1.0, fm_amount: 0.0,
                    mod_slots: [ModSlot::default(); 2], mod_wheel: 0.0, aftertouch: 0.0,
                };
                let (wl, wr) = v.wave.process(params);
                sl += wl * v.env;
                sr += wr * v.env;
            } else if instrument == INSTR_KUBYZ {
                // Kubyz jaw-harp — superduper-kubyz DSP. Built-in Bashkir timbre; the Motion param
                // (mod_cents 0..60) opens the formant mix for a vowel sweep.
                let params = KubyzParams {
                    sr: srate, root_hz: f, harmonics: &e.kubyz_harm,
                    formant_f: KUBYZ_FORMANT_F, formant_bw: KUBYZ_FORMANT_BW, formant_gain: KUBYZ_FORMANT_GAIN,
                    formant_mix: (mod_cents / 60.0).clamp(0.0, 1.0), velocity_formant_shift: 0.1,
                };
                let (kl, kr) = v.kubyz.process(params);
                sl += kl * v.env;
                sr += kr * v.env;
            } else {
                let base = PadParams {
                    sr: srate, root_hz: f, cutoff_hz: cutoff, resonance,
                    modulation_cents: mod_cents, drive,
                };
                let pr = PadParams { root_hz: f * 1.003, ..base }; // slight L/R detune for width
                sl += v.l.process(base) * v.env;
                sr += v.r.process(pr) * v.env;
            }
        }
        } // end else (poly pool)
        // Dry stereo, then the selected FX slot (voices borrow released → can borrow reverb/filters).
        // Drums are already a finished mix, so they bypass the 0.22 poly-sum gain.
        let g = if instrument == INSTR_DRUM { 1.0 } else { 0.22 };
        let mut dl = sl * g;
        let mut dr = sr * g;
        match fx {
            FX_REVERB => {
                let mut wet = [0.0_f32; 2];
                e.reverb.tick(&[dl, dr], &mut wet);
                dl = dl * (1.0 - amt) + wet[0] * amt;
                dr = dr * (1.0 - amt) + wet[1] * amt;
            }
            FX_FILTER => {
                let cut = 200.0 * 80.0_f32.powf(amt); // amt 0→200Hz, 1→16kHz (open)
                dl = e.filt_l.process(dl, srate, cut);
                dr = e.filt_r.process(dr, srate, cut);
            }
            FX_SATURATE => {
                let drv = 1.0 + amt * 8.0;
                let comp = 1.0 / (1.0 + amt * 1.5); // keep level roughly constant as drive rises
                dl = tanh_drive(dl, drv) * comp;
                dr = tanh_drive(dr, drv) * comp;
            }
            FX_DELAY => {
                let dt = 0.32 * srate; // 320 ms
                let (wl, wr) = (e.delay_l.read_lagrange3(dt), e.delay_r.read_lagrange3(dt));
                e.delay_l.write(dl + wr * 0.45); // cross-feed → ping-pong
                e.delay_r.write(dr + wl * 0.45);
                dl = dl * (1.0 - amt) + wl * amt;
                dr = dr * (1.0 - amt) + wr * amt;
            }
            FX_CHORUS => {
                e.chorus_phase += 0.8 / srate; // ~0.8 Hz
                if e.chorus_phase >= 1.0 { e.chorus_phase -= 1.0; }
                let lfo = (e.chorus_phase * core::f32::consts::TAU).sin();
                let dt = (0.012 + 0.004 * lfo) * srate; // 12 ± 4 ms
                let (wl, wr) = (e.delay_l.read_lagrange3(dt), e.delay_r.read_lagrange3(dt));
                e.delay_l.write(dl);
                e.delay_r.write(dr);
                dl = dl * (1.0 - amt * 0.5) + wl * amt * 0.5;
                dr = dr * (1.0 - amt * 0.5) + wr * amt * 0.5;
            }
            _ => {}
        }
        ol[i] = dl.clamp(-1.0, 1.0);
        or[i] = dr.clamp(-1.0, 1.0);
    }
}
