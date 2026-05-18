//! SuperDuper Wave — wavetable bass synth with math-formula presets.
//!
//! Architecture mirrors Pad: 8-voice pool, sample-accurate MIDI batching,
//! click-free voice steal, soft-fade choke. The DSP differs — instead of a
//! 4-partial pad oscillator each voice has a unison fan-out of wavetable
//! oscillators (reading two formula-baked frames blended by WT Pos) plus
//! a sub octave, an SVF filter and an ADSR.
//!
//! Presets are pure math: `fn(phase: f32) -> f32`. See `presets.rs`.

#![allow(clippy::missing_safety_doc)]

// Formant DSP block lives in synth-core now (`superduper_synth_core::formant`)
// — Kubyz uses it directly. Wave dropped its formant stage after user
// feedback (too subtle on bass voices).
pub mod gui;
pub mod osc;
pub mod presets;

use atomic_float::AtomicF32;
use clack_common::events::Match;
use clack_common::events::spaces::CoreEventSpace;
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams,
    PluginParams,
};
use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use parking_lot::Mutex;
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{AdsrEnvelope, AdsrParams, SmoothedParam, midi_note_to_hz};

use osc::{FilterMode, LfoDest, LfoShape, MipWavetable, WaveParams, WaveVoice, NOTE_FREE};
use presets::PRESETS;

// ---------------------------------------------------------------------------
// Params — 12 controls. Layout sits next to the GUI section grouping.
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0,  name: b"WT Pos",     min: 0.0,    max: 1.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 1,  name: b"Unison",     min: 1.0,    max: 7.0,     default: 1.0,    unit: ""     },
    ParamDef { id: 2,  name: b"Detune",     min: 0.0,    max: 50.0,    default: 0.0,    unit: "ct"   },
    ParamDef { id: 3,  name: b"Sub",        min: 0.0,    max: 1.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 4,  name: b"Cutoff",     min: 30.0,   max: 18000.0, default: 4000.0, unit: "Hz"   },
    ParamDef { id: 5,  name: b"Resonance",  min: 0.0,    max: 0.9,     default: 0.2,    unit: ""     },
    ParamDef { id: 6,  name: b"Filter",     min: 0.0,    max: 2.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 7,  name: b"Drive",      min: 0.0,    max: 1.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 8,  name: b"Attack",     min: 0.001,  max: 4.0,     default: 0.005,  unit: "s"    },
    ParamDef { id: 9,  name: b"Decay",      min: 0.01,   max: 4.0,     default: 0.4,    unit: "s"    },
    ParamDef { id: 10, name: b"Sustain",    min: 0.0,    max: 1.0,     default: 0.8,    unit: ""     },
    ParamDef { id: 11, name: b"Release",    min: 0.01,   max: 8.0,     default: 0.3,    unit: "s"    },
    ParamDef { id: 12, name: b"Output",     min: -36.0,  max: 6.0,     default: -8.0,   unit: "dB"   },
    // Mip-mapped anti-aliasing toggle — 0 = raw wavetable read (full
    // bandwidth, audible aliasing on high notes), 1 = pick band-limited
    // mip per-voice (clean but slightly duller HF on bass). Exposed as a
    // param so the user can A/B in the host.
    ParamDef { id: 13, name: b"Anti-Alias", min: 0.0,    max: 1.0,     default: 1.0,    unit: ""     },
    // ---- Noise oscillator ----
    ParamDef { id: 14, name: b"Noise",      min: 0.0,    max: 1.0,     default: 0.0,    unit: ""     },
    // ---- Filter envelope (separate ADSR routed to cutoff in octaves) ----
    ParamDef { id: 15, name: b"FEnv Amt",   min: -6.0,   max: 6.0,     default: 0.0,    unit: "oct"  },
    ParamDef { id: 16, name: b"FEnv A",     min: 0.001,  max: 4.0,     default: 0.005,  unit: "s"    },
    ParamDef { id: 17, name: b"FEnv D",     min: 0.01,   max: 4.0,     default: 0.4,    unit: "s"    },
    ParamDef { id: 18, name: b"FEnv S",     min: 0.0,    max: 1.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 19, name: b"FEnv R",     min: 0.01,   max: 8.0,     default: 0.3,    unit: "s"    },
    // ---- LFO 1 ----
    ParamDef { id: 20, name: b"LFO Rate",   min: 0.05,   max: 30.0,    default: 4.0,    unit: "Hz"   },
    ParamDef { id: 21, name: b"LFO Depth",  min: 0.0,    max: 1.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 22, name: b"LFO Shape",  min: 0.0,    max: 3.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 23, name: b"LFO Dest",   min: 0.0,    max: 2.0,     default: 0.0,    unit: ""     },
];

pub const P_WT_POS: usize = 0;
pub const P_UNISON: usize = 1;
pub const P_DETUNE: usize = 2;
pub const P_SUB: usize = 3;
pub const P_CUTOFF: usize = 4;
pub const P_RESONANCE: usize = 5;
pub const P_FILTER_MODE: usize = 6;
pub const P_DRIVE: usize = 7;
pub const P_ATTACK: usize = 8;
pub const P_DECAY: usize = 9;
pub const P_SUSTAIN: usize = 10;
pub const P_RELEASE: usize = 11;
pub const P_OUTPUT: usize = 12;
pub const P_ANTIALIAS: usize = 13;
pub const P_NOISE: usize = 14;
pub const P_FENV_AMOUNT: usize = 15;
pub const P_FENV_A: usize = 16;
pub const P_FENV_D: usize = 17;
pub const P_FENV_S: usize = 18;
pub const P_FENV_R: usize = 19;
pub const P_LFO_RATE: usize = 20;
pub const P_LFO_DEPTH: usize = 21;
pub const P_LFO_SHAPE: usize = 22;
pub const P_LFO_DEST: usize = 23;

pub const VOICE_COUNT: usize = 8;

// ---------------------------------------------------------------------------
// Shared params + currently-loaded wavetable handles.
//
// Preset changes are coordinated through `pending_preset` (main thread sets
// it, audio thread swaps `frame_a` / `frame_b` at the next render block).
// Wavetable handles are `Arc<[f32; WT_SIZE]>` — cheap pointer-only swap.
// ---------------------------------------------------------------------------

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: AtomicBool,
    pub dirty_params: [AtomicBool; PARAMS.len()],
    pub active_voices: AtomicU32,
    /// Live polyphony index — written by GUI on preset pick, read by audio
    /// thread once per process() to swap the wavetable handles.
    pub pending_preset: AtomicU32,
    /// Set by GUI whenever it pushes a freshly-baked frame_a (custom-curve
    /// edits) or any other out-of-band wavetable change. Audio thread
    /// re-clones from `wavetable` on the next process() block.
    pub pending_swap: AtomicBool,
    /// Wavetable pair currently in use. Each entry is a mip pyramid —
    /// audio thread picks the right band-limited level per voice. The
    /// pyramid itself is rebuilt off the audio thread (preset apply or
    /// curve edit) and pointer-swapped here.
    pub wavetable: Mutex<(MipWavetable, MipWavetable)>,
    /// Active wavetable preset id — used by the GUI editor to decide
    /// whether to seed its nodes from the preset formula or from
    /// existing custom data.
    pub active_preset: AtomicU32,
}

pub struct PluginShared {
    pub inner: SharedParams,
}

impl PluginShared {
    pub fn new() -> Self {
        let init = &PRESETS[0];
        let frame_a = osc::render_formula_mip(init.frame_a);
        let frame_b = osc::render_formula_mip(init.frame_b);
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| AtomicBool::new(false)),
                active_voices: AtomicU32::new(0),
                pending_preset: AtomicU32::new(u32::MAX),
                pending_swap: AtomicBool::new(false),
                wavetable: Mutex::new((frame_a, frame_b)),
                active_preset: AtomicU32::new(0),
            }),
        }
    }
    pub fn shared_handle(&self) -> SharedParams {
        std::sync::Arc::clone(&self.inner)
    }
}

impl Default for PluginShared {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Deref for PluginShared {
    type Target = SharedParamsInner;
    fn deref(&self) -> &SharedParamsInner {
        &self.inner
    }
}
impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

/// Helper used by GUI: apply a preset's defaults atomically + queue the
/// wavetable swap. Audio thread will pick the new tables up at the next
/// process() call.
pub fn apply_preset(shared: &SharedParamsInner, preset_idx: usize) {
    let Some(preset) = PRESETS.get(preset_idx) else { return };
    let defaults = preset.default_values();
    for (i, &v) in defaults.iter().enumerate() {
        if let Some(atom) = shared.params.get(i) {
            atom.store(v, Ordering::Relaxed);
        }
    }
    let frame_a = osc::render_formula_mip(preset.frame_a);
    let frame_b = osc::render_formula_mip(preset.frame_b);
    {
        let mut guard = shared.wavetable.lock();
        *guard = (frame_a, frame_b);
    }
    shared.active_preset.store(preset_idx as u32, Ordering::Relaxed);
    shared.pending_preset.store(preset_idx as u32, Ordering::Relaxed);
    shared.pending_swap.store(true, Ordering::Relaxed);
}

/// Replace just `frame_a` of the active wavetable — used by the GUI's
/// custom-curve editor (each mouse move re-bakes a fresh pyramid).
/// `frame_b` stays at whatever the preset put there, so WT Pos morphs
/// from the edited curve toward the preset's second frame.
pub fn push_custom_frame_a(shared: &SharedParamsInner, new_frame_a: MipWavetable) {
    {
        let mut guard = shared.wavetable.lock();
        guard.0 = new_frame_a;
    }
    shared.pending_swap.store(true, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Main-thread state + audio processor.
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    voices: [WaveVoice; VOICE_COUNT],
    /// Local wavetable handle — clone-on-swap from `shared.wavetable` so the
    /// audio thread renders without taking the mutex on every sample.
    frame_a: MipWavetable,
    frame_b: MipWavetable,
    /// Previous `frame_a` kept around for the crossfade — every wavetable
    /// edit (mouse-drag in the curve editor) used to click-swap and pop;
    /// with this we glide between the old and new tables over ~12 ms.
    frame_a_prev: MipWavetable,
    /// 0..1, where 1 = fully on new `frame_a`, 0 = still on `frame_a_prev`.
    fade_pos: f32,
    fade_inc: f32,
    next_age: u64,
    smooth_wt_pos: SmoothedParam,
    smooth_detune: SmoothedParam,
    smooth_sub: SmoothedParam,
    smooth_cutoff: SmoothedParam,
    smooth_resonance: SmoothedParam,
    smooth_drive: SmoothedParam,
    smooth_output: SmoothedParam,
    sample_rate: f32,
}

#[inline]
fn matches_key(target: Match<u16>, key: u8) -> bool {
    match target {
        Match::All => true,
        Match::Specific(k) => k as u8 == key,
    }
}

#[inline]
fn matches_note_id(target: Match<u32>, note_id: i32) -> bool {
    match target {
        Match::All => true,
        Match::Specific(id) => id as i32 == note_id,
    }
}

impl<'a> PluginAudioProcessor<'a> {
    fn allocate_voice(&mut self, key: u8, velocity: f32, note_id: i32) {
        self.next_age = self.next_age.wrapping_add(1);
        let stamp = self.next_age;

        let unison = self.shared.params[P_UNISON].load(Ordering::Relaxed) as u32;
        let detune = self.shared.params[P_DETUNE].load(Ordering::Relaxed);

        // 1. Retrigger same key — single voice per held note.
        for v in self.voices.iter_mut() {
            if v.key == key && v.note_id == note_id {
                v.env.gate_on();
                v.velocity = velocity;
                v.age_stamp = stamp;
                v.choke_remaining = 0;
                v.configure_unison(unison, detune);
                return;
            }
        }
        // 2. Free voice — re-use oscillator state for click-free attack.
        if let Some(v) = self.voices.iter_mut().find(|v| v.env.is_idle() && v.choke_remaining == 0) {
            v.key = key;
            v.note_id = note_id;
            v.velocity = velocity;
            v.age_stamp = stamp;
            v.env = AdsrEnvelope::default();
            v.env.gate_on();
            v.filter_env = AdsrEnvelope::default();
            v.filter_env.gate_on();
            v.lfo_phase = 0.0;
            v.choke_remaining = 0;
            v.configure_unison(unison, detune);
            v.scatter_phases();
            return;
        }
        // 3. Steal — quietest releasing, else oldest.
        let mut steal_idx = 0usize;
        let mut steal_score = f32::INFINITY;
        let mut found_release = false;
        for (i, v) in self.voices.iter().enumerate() {
            if v.env.is_releasing() {
                let lvl = v.env.level();
                if lvl < steal_score {
                    steal_score = lvl;
                    steal_idx = i;
                    found_release = true;
                }
            }
        }
        if !found_release {
            let mut oldest = u64::MAX;
            for (i, v) in self.voices.iter().enumerate() {
                if v.age_stamp < oldest {
                    oldest = v.age_stamp;
                    steal_idx = i;
                }
            }
        }
        let v = &mut self.voices[steal_idx];
        v.key = key;
        v.note_id = note_id;
        v.velocity = velocity;
        v.age_stamp = stamp;
        v.choke_remaining = 0;
        v.env.gate_on();
        v.filter_env = AdsrEnvelope::default();
        v.filter_env.gate_on();
        v.lfo_phase = 0.0;
        v.configure_unison(unison, detune);
    }

    fn release_voice(&mut self, key_match: Match<u16>, note_id_match: Match<u32>) {
        for v in self.voices.iter_mut() {
            if v.key == NOTE_FREE {
                continue;
            }
            if matches_key(key_match, v.key) && matches_note_id(note_id_match, v.note_id) {
                v.env.gate_off();
                v.filter_env.gate_off();
            }
        }
    }

    fn choke_voice(&mut self, key_match: Match<u16>, note_id_match: Match<u32>) {
        let fade_samples = (self.sample_rate * 0.005) as u32;
        for v in self.voices.iter_mut() {
            if v.key == NOTE_FREE && v.choke_remaining == 0 {
                continue;
            }
            if matches_key(key_match, v.key) && matches_note_id(note_id_match, v.note_id) {
                v.choke_level = v.env.level();
                v.choke_total = fade_samples.max(1);
                v.choke_remaining = v.choke_total;
            }
        }
    }

    fn handle_midi_event(&mut self, data: [u8; 3]) {
        let status = data[0] & 0xf0;
        let key = data[1];
        let raw_velocity = data[2];
        match status {
            0x90 => {
                if raw_velocity == 0 {
                    self.release_voice(Match::Specific(key as u16), Match::All);
                } else {
                    let velocity = raw_velocity as f32 / 127.0;
                    self.allocate_voice(key, velocity, -1);
                }
            }
            0x80 => self.release_voice(Match::Specific(key as u16), Match::All),
            0xb0 if key == 123 => self.release_voice(Match::All, Match::All),
            0xb0 if key == 120 => self.choke_voice(Match::All, Match::All),
            _ => {}
        }
    }

    fn handle_note_event(&mut self, ev: &CoreEventSpace<'_>) {
        match ev {
            CoreEventSpace::NoteOn(n) => {
                let key = match n.key() {
                    Match::Specific(k) => k as u8,
                    Match::All => return,
                };
                let velocity = n.velocity().clamp(0.0, 1.0) as f32;
                let note_id = match n.note_id() {
                    Match::Specific(id) => id as i32,
                    Match::All => -1,
                };
                self.allocate_voice(key, velocity, note_id);
            }
            CoreEventSpace::NoteOff(n) => self.release_voice(n.key(), n.note_id()),
            CoreEventSpace::NoteChoke(n) => self.choke_voice(n.key(), n.note_id()),
            CoreEventSpace::Midi(m) => self.handle_midi_event(m.data()),
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_subblock(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        wt_pos_target: f32,
        unison: u32,
        detune_target: f32,
        sub_target: f32,
        cutoff_target: f32,
        resonance_target: f32,
        mode: FilterMode,
        drive_target: f32,
        output_target: f32,
        attack_s: f32,
        decay_s: f32,
        sustain: f32,
        release_s: f32,
        antialias: bool,
        noise_level: f32,
        fenv_amount_oct: f32,
        fenv_a: f32,
        fenv_d: f32,
        fenv_s: f32,
        fenv_r: f32,
        lfo_rate_hz: f32,
        lfo_depth: f32,
        lfo_shape: LfoShape,
        lfo_dest: LfoDest,
    ) {
        let sr = self.sample_rate;
        debug_assert_eq!(out_l.len(), out_r.len());

        for i in 0..out_l.len() {
            let wt_pos = self.smooth_wt_pos.step(wt_pos_target, sr).clamp(0.0, 1.0);
            let detune = self.smooth_detune.step(detune_target, sr);
            let sub_level = self.smooth_sub.step(sub_target, sr).clamp(0.0, 1.0);
            let cutoff = self.smooth_cutoff.step(cutoff_target, sr);
            let resonance = self.smooth_resonance.step(resonance_target, sr).clamp(0.0, 0.95);
            let drive = self.smooth_drive.step(drive_target, sr).clamp(0.0, 1.0);
            let output_db = self.smooth_output.step(output_target, sr);

            // Recompute unison spread if Detune changed materially — saves
            // configure_unison() per sample. Cheap heuristic: only redo when
            // the slewed value diverges from each voice's baked detune by
            // more than 0.5 cent. For now: configure once per sample (still
            // fast — 7 trig ops). Optimise if profiler complains.
            for v in self.voices.iter_mut() {
                if v.key != NOTE_FREE || !v.env.is_idle() || v.choke_remaining > 0 {
                    v.configure_unison(unison, detune);
                }
            }

            let adsr_p = AdsrParams { sr, attack_s, decay_s, sustain, release_s };

            let mut mix_l = 0.0_f32;
            let mut mix_r = 0.0_f32;
            for v in self.voices.iter_mut() {
                if v.key == NOTE_FREE && v.env.is_idle() && v.choke_remaining == 0 {
                    continue;
                }
                // Advance the crossfade once per output sample so every
                // voice (and the choke-fade above) sees the same fade pos.
                self.fade_pos = (self.fade_pos + self.fade_inc).min(1.0);
                let fade_pos = self.fade_pos;

                let base_hz = midi_note_to_hz(v.key as f32);
                let params = WaveParams {
                    sr,
                    root_hz: base_hz,
                    wt_pos,
                    unison,
                    detune_cents: detune,
                    sub_level,
                    noise_level,
                    cutoff_hz: cutoff,
                    resonance,
                    mode,
                    drive,
                    antialias,
                    fenv_amount_oct,
                    fenv: AdsrParams {
                        sr,
                        attack_s: fenv_a,
                        decay_s: fenv_d,
                        sustain: fenv_s,
                        release_s: fenv_r,
                    },
                    lfo_shape,
                    lfo_dest,
                    lfo_rate_hz,
                    lfo_depth,
                    frame_a: &self.frame_a,
                    frame_a_prev: &self.frame_a_prev,
                    frame_a_fade: fade_pos,
                    frame_b: &self.frame_b,
                };

                // Choke fade — overrides ADSR with linear ramp.
                if v.choke_remaining > 0 {
                    let fade = (v.choke_remaining as f32) / (v.choke_total as f32);
                    let (l, r) = v.process(params);
                    let amp = fade * v.choke_level * v.velocity;
                    mix_l += l * amp;
                    mix_r += r * amp;
                    v.choke_remaining -= 1;
                    if v.choke_remaining == 0 {
                        v.env = AdsrEnvelope::default();
                        v.key = NOTE_FREE;
                    }
                    continue;
                }

                let env = v.env.process(adsr_p);
                if env <= 1e-5 && v.env.is_idle() {
                    v.key = NOTE_FREE;
                    continue;
                }
                let (l, r) = v.process(params);
                let amp = env * v.velocity;
                mix_l += l * amp;
                mix_r += r * amp;
            }

            let voice_scale = 0.5_f32;
            let out_lin = 10f32.powf(output_db / 20.0);
            out_l[i] = mix_l * voice_scale * out_lin;
            out_r[i] = mix_r * voice_scale * out_lin;
        }
    }

    fn count_active(&self) -> u32 {
        self.voices
            .iter()
            .filter(|v| !v.env.is_idle() || v.key != NOTE_FREE || v.choke_remaining > 0)
            .count() as u32
    }

    /// Pick up pending wavetable changes (preset apply OR custom-curve push)
    /// and start a crossfade from the previous `frame_a` so the swap is
    /// inaudible.  Runs once per process() — Arc::ptr_eq guards against
    /// pointless fades when nothing actually moved.
    fn maybe_swap_wavetable(&mut self) {
        if self.shared.pending_swap.swap(false, Ordering::AcqRel) {
            let guard = self.shared.wavetable.lock();
            let new_a = guard.0.clone();
            let new_b = guard.1.clone();
            drop(guard);
            // Cheap "did frame_a actually move" check via the level-0 Arc
            // pointer — saves an unnecessary crossfade when only frame_b
            // changed (preset swaps still cover both via pending_swap).
            if !std::sync::Arc::ptr_eq(&new_a.levels[0], &self.frame_a.levels[0]) {
                self.frame_a_prev = std::mem::replace(&mut self.frame_a, new_a);
                self.fade_pos = 0.0;
            }
            self.frame_b = new_b;
            self.shared.pending_preset.store(u32::MAX, Ordering::Relaxed);
        }
    }
}

impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, PluginShared, PluginMainThread<'a>>
    for PluginAudioProcessor<'a>
{
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut PluginMainThread<'a>,
        shared: &'a PluginShared,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        let (frame_a, frame_b) = {
            let guard = shared.wavetable.lock();
            (guard.0.clone(), guard.1.clone())
        };
        // 12 ms crossfade between old/new frame_a — short enough to feel
        // immediate, long enough to bury the curve-edit zipper noise.
        let fade_samples = (sr * 0.012).max(1.0);
        let fade_inc = 1.0 / fade_samples;
        let frame_a_prev = frame_a.clone();
        Ok(Self {
            shared,
            voices: std::array::from_fn(|_| WaveVoice::default()),
            frame_a,
            frame_b,
            frame_a_prev,
            fade_pos: 1.0,
            fade_inc,
            next_age: 0,
            smooth_wt_pos: SmoothedParam::new(load(P_WT_POS)),
            smooth_detune: SmoothedParam::new(load(P_DETUNE)),
            smooth_sub: SmoothedParam::new(load(P_SUB)),
            smooth_cutoff: SmoothedParam::new(load(P_CUTOFF)),
            smooth_resonance: SmoothedParam::new(load(P_RESONANCE)),
            smooth_drive: SmoothedParam::new(load(P_DRIVE)),
            smooth_output: SmoothedParam::new(load(P_OUTPUT)),
            sample_rate: sr,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // Flush GUI-driven param changes back to the host so REAPER can
        // record the move into the automation lane.
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(
            &self.shared.params, &self.shared.dirty_params, events.output);
        self.maybe_swap_wavetable();

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else {
                continue;
            };
            let mut writers: Vec<_> = channel_pairs
                .into_iter()
                .filter_map(superduper_dsp_sdk::clap_helpers::output_slice)
                .collect();
            if writers.len() < 2 {
                for w in writers.iter_mut() {
                    w.fill(0.0);
                }
                continue;
            }

            let (a, b) = writers.split_at_mut(1);
            let out_l: &mut [f32] = a[0];
            let out_r: &mut [f32] = b[0];
            let frames = out_l.len().min(out_r.len());

            if bypassed {
                out_l[..frames].fill(0.0);
                out_r[..frames].fill(0.0);
                continue;
            }

            for batch in events.input.batch() {
                for ev in batch.events() {
                    if let Some(core) = ev.as_core_event() {
                        match core {
                            CoreEventSpace::ParamValue(pv) => {
                                if let Some(id) = pv.param_id() {
                                    let idx = id.get() as usize;
                                    if let Some(atom) = self.shared.params.get(idx) {
                                        atom.store(pv.value() as f32, Ordering::Relaxed);
                                    }
                                }
                            }
                            _ => self.handle_note_event(&core),
                        }
                    }
                }

                let start = batch.first_sample().min(frames);
                let end = batch.next_batch_first_sample().unwrap_or(frames).min(frames);
                if end <= start {
                    continue;
                }

                let wt_pos = self.shared.params[P_WT_POS].load(Ordering::Relaxed);
                let unison = self.shared.params[P_UNISON].load(Ordering::Relaxed) as u32;
                let detune = self.shared.params[P_DETUNE].load(Ordering::Relaxed);
                let sub = self.shared.params[P_SUB].load(Ordering::Relaxed);
                let cutoff = self.shared.params[P_CUTOFF].load(Ordering::Relaxed);
                let resonance = self.shared.params[P_RESONANCE].load(Ordering::Relaxed);
                let mode = FilterMode::from_index(
                    self.shared.params[P_FILTER_MODE].load(Ordering::Relaxed) as u32,
                );
                let drive = self.shared.params[P_DRIVE].load(Ordering::Relaxed);
                let output = self.shared.params[P_OUTPUT].load(Ordering::Relaxed);
                let attack = self.shared.params[P_ATTACK].load(Ordering::Relaxed);
                let decay = self.shared.params[P_DECAY].load(Ordering::Relaxed);
                let sustain = self.shared.params[P_SUSTAIN].load(Ordering::Relaxed);
                let release = self.shared.params[P_RELEASE].load(Ordering::Relaxed);

                let antialias =
                    self.shared.params[P_ANTIALIAS].load(Ordering::Relaxed) >= 0.5;
                let noise_level = self.shared.params[P_NOISE].load(Ordering::Relaxed);
                let fenv_amount_oct = self.shared.params[P_FENV_AMOUNT].load(Ordering::Relaxed);
                let fenv_a = self.shared.params[P_FENV_A].load(Ordering::Relaxed);
                let fenv_d = self.shared.params[P_FENV_D].load(Ordering::Relaxed);
                let fenv_s = self.shared.params[P_FENV_S].load(Ordering::Relaxed);
                let fenv_r = self.shared.params[P_FENV_R].load(Ordering::Relaxed);
                let lfo_rate = self.shared.params[P_LFO_RATE].load(Ordering::Relaxed);
                let lfo_depth = self.shared.params[P_LFO_DEPTH].load(Ordering::Relaxed);
                let lfo_shape = LfoShape::from_index(
                    self.shared.params[P_LFO_SHAPE].load(Ordering::Relaxed) as u32,
                );
                let lfo_dest = LfoDest::from_index(
                    self.shared.params[P_LFO_DEST].load(Ordering::Relaxed) as u32,
                );

                self.render_subblock(
                    &mut out_l[start..end],
                    &mut out_r[start..end],
                    wt_pos, unison, detune, sub, cutoff, resonance, mode, drive, output,
                    attack, decay, sustain, release,
                    antialias,
                    noise_level,
                    fenv_amount_oct, fenv_a, fenv_d, fenv_s, fenv_r,
                    lfo_rate, lfo_depth, lfo_shape, lfo_dest,
                );
            }

            if writers.len() > 2 {
                for w in writers.iter_mut().skip(2) {
                    w.fill(0.0);
                }
            }
        }

        self.shared
            .active_voices
            .store(self.count_active(), Ordering::Relaxed);

        Ok(ProcessStatus::Continue)
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions (mirrors Pad)
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input { 0 } else { 1 }
    }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if is_input || index != 0 {
            return;
        }
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: b"Output",
            channel_count: 2,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            in_place_pair: None,
        });
    }
}

impl PluginNotePortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input { 1 } else { 0 }
    }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if !is_input || index != 0 {
            return;
        }
        writer.set(&NotePortInfo {
            id: ClapId::new(0),
            name: b"Notes",
            supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
            preferred_dialect: Some(NoteDialect::Clap),
        });
    }
}

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 {
        PARAMS.len() as u32
    }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info(PARAMS, idx, info);
    }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        let i = id.get() as usize;
        self.shared.params.get(i).map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(&mut self, id: ClapId, v: f64, w: &mut ParamDisplayWriter) -> core::fmt::Result {
        ParamDef::write_display(PARAMS, id, v, w)
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        ParamDef::parse_text(PARAMS, id, t)
    }
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
    }
}

use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window as ClapGuiWindow,
};

impl PluginGuiImpl for PluginMainThread<'_> {
    fn is_api_supported(&mut self, c: GuiConfiguration) -> bool {
        if c.is_floating {
            return false;
        }
        c.api_type == GuiApiType::COCOA
            || c.api_type == GuiApiType::WIN32
            || c.api_type == GuiApiType::X11
    }
    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        let api_type = if cfg!(target_os = "macos") {
            GuiApiType::COCOA
        } else if cfg!(target_os = "windows") {
            GuiApiType::WIN32
        } else {
            GuiApiType::X11
        };
        Some(GuiConfiguration { api_type, is_floating: false })
    }
    fn create(&mut self, _: GuiConfiguration) -> Result<(), PluginError> { Ok(()) }
    fn destroy(&mut self) { self.gui_handle = None; }
    fn set_scale(&mut self, _: f64) -> Result<(), PluginError> { Ok(()) }
    fn get_size(&mut self) -> Option<GuiSize> {
        Some(GuiSize {
            width: self.gui_resize.0.load(Ordering::Relaxed),
            height: self.gui_resize.1.load(Ordering::Relaxed),
        })
    }
    fn can_resize(&mut self) -> bool { true }
    fn get_resize_hints(&mut self) -> Option<GuiResizeHints> {
        Some(GuiResizeHints {
            can_resize_horizontally: true,
            can_resize_vertically: true,
            strategy: AspectRatioStrategy::Disregard,
        })
    }
    fn adjust_size(&mut self, s: GuiSize) -> Option<GuiSize> {
        Some(GuiSize {
            width: s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH),
            height: s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT),
        })
    }
    fn set_size(&mut self, s: GuiSize) -> Result<(), PluginError> {
        let w = s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH);
        let h = s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT);
        self.gui_resize.0.store(w, Ordering::Relaxed);
        self.gui_resize.1.store(h, Ordering::Relaxed);
        Ok(())
    }
    fn set_parent(&mut self, window: ClapGuiWindow) -> Result<(), PluginError> {
        let handle = gui::open_window(&window, self.shared.shared_handle(), self.gui_resize.clone());
        self.gui_handle = Some(handle);
        Ok(())
    }
    fn set_transient(&mut self, _: ClapGuiWindow) -> Result<(), PluginError> { Ok(()) }
    fn show(&mut self) -> Result<(), PluginError> { Ok(()) }
    fn hide(&mut self) -> Result<(), PluginError> { Ok(()) }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct SuperDuperWave;

impl Plugin for SuperDuperWave {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;
    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>()
            .register::<clack_extensions::gui::PluginGui>();
    }
}

impl DefaultPluginFactory for SuperDuperWave {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.wave",
            plugin_display_name!("SuperDuper Wave"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Wavetable bass synth with math-formula presets")
        .with_features([INSTRUMENT, STEREO, SYNTHESIZER])
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        Ok(PluginShared::new())
    }
    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        shared: &'a PluginShared,
    ) -> Result<PluginMainThread<'a>, PluginError> {
        Ok(PluginMainThread {
            shared,
            gui_handle: None,
            gui_resize: gui::new_resize_bridge(),
        })
    }
}

clack_export_entry!(SinglePluginEntry<SuperDuperWave>);

// Silence unused-build-macro warnings when sdk is updated.
#[allow(dead_code)]
fn _meta() -> (&'static str, &'static str) {
    (build_num!(), build_date!())
}
