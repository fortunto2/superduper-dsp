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

use superduper_synth_core::dsp_blocks::{midi_note_to_hz, PadParams, PadVoice};

const MAX_VOICES: usize = 16;

struct Voice {
    l: PadVoice,
    r: PadVoice,
    key: u8,
    gate: bool, // note currently held
    env: f32,   // 0..1 AR envelope (PadVoice has no envelope of its own)
}

impl Voice {
    fn new() -> Self {
        Self { l: PadVoice::default(), r: PadVoice::default(), key: 255, gate: false, env: 0.0 }
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
    mod_cents: f32, mod_cents_t: f32, // LFO detune depth
}

#[no_mangle]
pub extern "C" fn sdsp_create(sample_rate: f32) -> *mut Engine {
    let e = Box::new(Engine {
        sr: if sample_rate > 0.0 { sample_rate } else { 48_000.0 },
        voices: (0..MAX_VOICES).map(|_| Voice::new()).collect(),
        cutoff: 2_400.0, cutoff_t: 2_400.0,
        resonance: 0.30, resonance_t: 0.30,
        drive: 0.25, drive_t: 0.25,
        mod_cents: 25.0, mod_cents_t: 25.0,
    });
    Box::into_raw(e)
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
    // Reuse a held voice on the same key, else the most-released (quietest) slot.
    let idx = e.voices.iter().position(|v| v.gate && v.key == key)
        .or_else(|| e.voices.iter().position(|v| !v.gate && v.env < 0.01))
        .unwrap_or_else(|| {
            e.voices.iter().enumerate()
                .min_by(|a, b| a.1.env.partial_cmp(&b.1.env).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(i, _)| i).unwrap_or(0)
        });
    let v = &mut e.voices[idx];
    if v.env < 0.01 { v.l = PadVoice::default(); v.r = PadVoice::default(); } // fresh slot → no click
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
        3 => e.mod_cents_t = v * 60.0,
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
        let (cutoff, resonance, drive, mod_cents) = (e.cutoff, e.resonance, e.drive, e.mod_cents);
        let mut sl = 0.0_f32;
        let mut sr = 0.0_f32;
        for v in e.voices.iter_mut() {
            let target = if v.gate { 1.0 } else { 0.0 };
            let rate = if v.gate { atk } else { rel };
            v.env += (target - v.env) * rate;
            if !v.gate && v.env < 0.0005 { continue; }
            let f = midi_note_to_hz(v.key as f32);
            let base = PadParams {
                sr: e.sr, root_hz: f, cutoff_hz: cutoff, resonance,
                modulation_cents: mod_cents, drive,
            };
            // Slight L/R detune for stereo width.
            let pr = PadParams { root_hz: f * 1.003, ..base };
            sl += v.l.process(base) * v.env;
            sr += v.r.process(pr) * v.env;
        }
        ol[i] = (sl * 0.22).clamp(-1.0, 1.0);
        or[i] = (sr * 0.22).clamp(-1.0, 1.0);
    }
}
