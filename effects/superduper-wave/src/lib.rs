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
// The wavetable DSP moved to synth-core (so iOS/live2play reuses it). Re-export under the old
// `osc` path so every `crate::osc::…` / `osc::…` reference keeps working unchanged. The old
// src/osc.rs is now dead (left in place; delete on request).
pub use superduper_synth_core::wave_osc as osc;
pub mod presets;
pub mod user_extra;

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
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_common::stream::{InputStream, OutputStream};
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
    ParamDef { id: 24, name: b"Bend Range", min: 0.0,    max: 24.0,    default: 2.0,    unit: "ST"   },
    ParamDef { id: 25, name: b"LFO Sync",   min: 0.0,    max: 1.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 26, name: b"LFO Div",    min: 0.0,    max: 11.0,    default: 7.0,    unit: ""     },
    // ---- Mod matrix — 2 slots, each (src, dst, amt) ----
    // Source enum: 0=None, 1=LFO, 2=Velocity, 3=ModWheel, 4=Aftertouch, 5=FEnv
    // Dest enum:   0=None, 1=Cutoff(oct), 2=Pitch(ST), 3=WT Pos, 4=Reson, 5=Drive, 6=Volume
    ParamDef { id: 27, name: b"Mod1 Src",   min: 0.0,    max: 5.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 28, name: b"Mod1 Dst",   min: 0.0,    max: 6.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 29, name: b"Mod1 Amt",   min: -1.0,   max: 1.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 30, name: b"Mod2 Src",   min: 0.0,    max: 5.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 31, name: b"Mod2 Dst",   min: 0.0,    max: 6.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 32, name: b"Mod2 Amt",   min: -1.0,   max: 1.0,     default: 0.0,    unit: ""     },
    // Hard sync — phantom master sine at root × Sync Ratio resets the
    // main oscillator phase when it crosses zero. Classic Serum / Diva
    // sync-saw / sync-square sound. Sync Ratio = 1.0 + Sync = 0 is a
    // no-op (defaults).
    ParamDef { id: 33, name: b"Sync",       min: 0.0,    max: 1.0,     default: 0.0,    unit: ""     },
    ParamDef { id: 34, name: b"Sync Ratio", min: 0.25,   max: 4.0,     default: 1.0,    unit: "x"    },
    // Phase modulation from a sidecar sine at root × FM Ratio. FM Amt
    // scales the depth (0..1 → ±π phase swing). Cheap, musical FM
    // without needing a separate oscillator block.
    ParamDef { id: 35, name: b"FM Ratio",   min: 0.25,   max: 8.0,     default: 2.0,    unit: "x"    },
    ParamDef { id: 36, name: b"FM Amt",     min: 0.0,    max: 1.0,     default: 0.0,    unit: ""     },
    // Preset / waveform selector — stepped 0..N-1. Set from the host or an
    // agent (producer-pal / MCP) to recall a preset *including its wavetable*
    // without opening the GUI. The recall (apply_preset) allocates mip tables,
    // so it runs on the main thread — see PluginMainThreadParams::flush.
    ParamDef { id: 37, name: b"Preset",     min: 0.0,    max: (PRESETS.len() - 1) as f64, default: 0.0, unit: "" },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[25, 26, 33, 37];

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
pub const P_BEND_RANGE: usize = 24;
pub const P_LFO_SYNC: usize = 25;
pub const P_LFO_DIV: usize = 26;
pub const P_MOD1_SRC: usize = 27;
pub const P_MOD1_DST: usize = 28;
pub const P_MOD1_AMT: usize = 29;
pub const P_MOD2_SRC: usize = 30;
pub const P_MOD2_DST: usize = 31;
pub const P_MOD2_AMT: usize = 32;
pub const P_SYNC: usize = 33;
pub const P_SYNC_RATIO: usize = 34;
pub const P_FM_RATIO: usize = 35;
pub const P_FM_AMT: usize = 36;
pub const P_PRESET: usize = 37;

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
    pub gesture_begin: [AtomicBool; PARAMS.len()],
    pub gesture_end: [AtomicBool; PARAMS.len()],
    pub active_voices: AtomicU32,
    /// Live polyphony index — written by GUI on preset pick, read by audio
    /// thread once per process() to swap the wavetable handles.
    pub pending_preset: AtomicU32,
    /// Set by GUI whenever it pushes a freshly-baked frame_a (custom-curve
    /// edits) or any other out-of-band wavetable change. Audio thread
    /// re-clones from `wavetable` on the next process() block.
    pub pending_swap: AtomicBool,
    /// Main/GUI thread → audio thread hand-off for a new wavetable. The audio
    /// thread used to `lock()` `wavetable` and clone the whole mip vector
    /// inside process(): a blocking wait on a thread that might be mid-FFT
    /// building the next pyramid (priority inversion), plus an allocation in
    /// the callback. Now the writer parks a ready copy here and the audio
    /// thread `try_lock`s and takes it.
    pub pending_frames: parking_lot::Mutex<Option<Vec<MipWavetable>>>,
    /// Wavetable frames currently in use — 1..=`FRAMES_MAX` mip
    /// pyramids. Audio thread picks the right band-limited level
    /// per voice and lerps between adjacent frames via the WT Pos
    /// param. The pyramids themselves are rebuilt off the audio
    /// thread (preset apply or curve edit) and pointer-swapped here.
    pub wavetable: Mutex<Vec<MipWavetable>>,
    /// Active wavetable preset id — used by the GUI editor to decide
    /// whether to seed its nodes from the preset formula or from
    /// existing custom data.
    pub active_preset: AtomicU32,
    /// Live pitch-bend in semitones (signed). Set by MIDI 0xE0.
    pub pitch_bend_st: AtomicF32,
    /// Live MIDI ModWheel (CC #1) value 0..1. Independent of the LFO Depth
    /// param so the mod matrix can pick it as a source even when CC1
    /// already happens to be routed to LFO Depth via the legacy mapping.
    pub mod_wheel: AtomicF32,
    /// Channel aftertouch 0..1. Same independence rationale as mod_wheel.
    pub aftertouch: AtomicF32,
    /// Live MIDI CC7 (Channel Volume) 0..1, default 1.0 (full). Drives a
    /// smoothed output VCA so a breath controller / mod wheel rig can swell
    /// volume expressively without touching the Output param.
    pub cc_volume: AtomicF32,
    /// Host transport BPM — updated from TransportEvent so the LFO can
    /// run in sync mode at musical divisions.
    pub host_bpm: AtomicF32,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub midi_learn: superduper_synth_core::gui::MidiLearnState,
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
                gesture_begin: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| AtomicBool::new(false)),
                active_voices: AtomicU32::new(0),
                pending_preset: AtomicU32::new(u32::MAX),
                pending_swap: AtomicBool::new(false),
                pending_frames: parking_lot::Mutex::new(None),
                wavetable: Mutex::new(vec![frame_a, frame_b]),
                active_preset: AtomicU32::new(0),
                pitch_bend_st: AtomicF32::new(0.0),
                mod_wheel: AtomicF32::new(0.0),
                aftertouch: AtomicF32::new(0.0),
                cc_volume: AtomicF32::new(1.0),
                host_bpm: AtomicF32::new(120.0),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
                midi_learn: superduper_synth_core::gui::MidiLearnState::new(),
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
        if i == P_PRESET { continue; } // never clobber the selector itself
        if let Some(atom) = shared.params.get(i) {
            atom.store(v, Ordering::Relaxed);
            // Mark dirty so the audio thread emits the new value back to the
            // host — otherwise the DAW's LOM/automation never sees the recall
            // (lesson 21d), and an agent reading params can't observe it.
            shared.dirty_params[i].store(true, Ordering::Relaxed);
        }
    }
    // Reflect the recalled preset back into the selector param so the host
    // and producer-pal/MCP read the active index, and record the switch.
    if let Some(atom) = shared.params.get(P_PRESET) {
        atom.store(preset_idx as f32, Ordering::Relaxed);
    }
    shared.dirty_params[P_PRESET].store(true, Ordering::Relaxed);
    let frame_a = osc::render_formula_mip(preset.frame_a);
    let frame_b = osc::render_formula_mip(preset.frame_b);
    {
        let frames = vec![frame_a, frame_b];
        *shared.pending_frames.lock() = Some(frames.clone());
        let mut guard = shared.wavetable.lock();
        *guard = frames;
    }
    shared.active_preset.store(preset_idx as u32, Ordering::Relaxed);
    shared.pending_preset.store(preset_idx as u32, Ordering::Relaxed);
    shared.pending_swap.store(true, Ordering::Relaxed);
}

/// Cap on user-extractable frame count. Past 16, the perception
/// gain is marginal and the mip-pyramid memory cost (10 levels ×
/// 2 KB per frame × N) starts to bite.
pub const FRAMES_MAX: usize = 16;

/// Replace just frames[0] of the active wavetable — used by the
/// GUI's custom-curve editor (each mouse move re-bakes a fresh
/// pyramid). Other frames stay in place, so WT Pos still morphs
/// from the edited curve through whatever else is loaded.
pub fn push_custom_frame_a(shared: &SharedParamsInner, new_frame_a: MipWavetable) {
    {
        let mut guard = shared.wavetable.lock();
        if guard.is_empty() {
            guard.push(new_frame_a);
        } else {
            guard[0] = new_frame_a;
        }
        *shared.pending_frames.lock() = Some(guard.clone());
    }
    shared.pending_swap.store(true, Ordering::Relaxed);
}

/// Replace ALL frames of the active wavetable in a single lock —
/// used by multi-frame WAV import + multi-frame preset load. The
/// Vec gets clamped to `FRAMES_MAX`; empty inputs are ignored.
pub fn push_custom_frames(
    shared: &SharedParamsInner,
    mut frames: Vec<MipWavetable>,
) {
    if frames.is_empty() {
        return;
    }
    if frames.len() > FRAMES_MAX {
        frames.truncate(FRAMES_MAX);
    }
    {
        *shared.pending_frames.lock() = Some(frames.clone());
        let mut guard = shared.wavetable.lock();
        *guard = frames;
    }
    shared.pending_swap.store(true, Ordering::Relaxed);
}

/// Deprecated 2-frame wrapper — kept for callers that still pass a
/// pair explicitly. New code should build a `Vec<MipWavetable>` and
/// use `push_custom_frames`.
pub fn push_custom_both_frames(
    shared: &SharedParamsInner,
    frame_a: MipWavetable,
    frame_b: MipWavetable,
) {
    push_custom_frames(shared, vec![frame_a, frame_b]);
}

// ---------------------------------------------------------------------------
// Main-thread state + audio processor.
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {
    /// Host main-thread callback — the audio thread calls request_callback()
    /// when the Preset param moved; here (main thread) the allocating recall
    /// is legal.
    fn on_main_thread(&mut self) {
        if let Some(idx) = superduper_dsp_sdk::clap_helpers::preset_recall_target(
            self.shared.params[P_PRESET].load(Ordering::Relaxed),
            &self.shared.active_preset,
            PRESETS.len(),
        ) {
            apply_preset(&self.shared.inner, idx);
        }
    }
}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    /// Host handle — used to wake the main thread (request_callback) when a
    /// Preset param change needs the (allocating) apply_preset recall.
    host: HostAudioProcessorHandle<'a>,
    voices: [WaveVoice; VOICE_COUNT],
    /// Local wavetable handle — clone-on-swap from `shared.wavetable`
    /// so the audio thread renders without taking the mutex on every
    /// sample. Length is 1..=`FRAMES_MAX`. WT Pos × (N-1) selects the
    /// integer pair to blend across; fractional part is the lerp.
    frames: Vec<MipWavetable>,
    /// Previous frames[0] kept around for the curve-edit crossfade —
    /// every mouse-drag re-bakes a new mip and used to click-swap
    /// and pop; with this we glide between old and new over ~12 ms.
    frame_a_prev: MipWavetable,
    /// 0..1, where 1 = fully on new frames[0], 0 = still on frame_a_prev.
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
    smooth_cc_vol: SmoothedParam,
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

        // 1. Retrigger same key — single voice per held note. Use `retrigger`
        // (legato re-attack from the current level), NOT `gate_on`: gate_on
        // routes through PreDelay which zeroes the level, dropping a still-
        // sounding held voice to silence for a sample → a click when the same
        // key is played again over a sustained drone.
        for v in self.voices.iter_mut() {
            if v.key == key && v.note_id == note_id {
                v.env.retrigger();
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
        // 3. Steal — quietest releasing, else oldest. Skip voices already in a
        // deferred-steal fade (`choke_remaining > 0`): re-choking a mid-fade
        // voice would reset its ramp to full level (a click) and clobber the
        // note already parked on it. A choking voice frees itself in ~4 ms.
        let mut steal_idx = 0usize;
        let mut steal_score = f32::INFINITY;
        let mut found_release = false;
        for (i, v) in self.voices.iter().enumerate() {
            if v.choke_remaining > 0 {
                continue;
            }
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
            let mut found = false;
            for (i, v) in self.voices.iter().enumerate() {
                if v.choke_remaining > 0 {
                    continue;
                }
                if v.age_stamp < oldest {
                    oldest = v.age_stamp;
                    steal_idx = i;
                    found = true;
                }
            }
            if !found {
                // Every voice is mid deferred-fade (extreme — 8 steals inside
                // 4 ms). Fall back to the oldest overall; a rare re-choke here
                // is preferable to dropping the note.
                let mut oldest = u64::MAX;
                for (i, v) in self.voices.iter().enumerate() {
                    if v.age_stamp < oldest {
                        oldest = v.age_stamp;
                        steal_idx = i;
                    }
                }
            }
        }
        // Deferred steal — the old note is at full amplitude, so a hard swap
        // (new pitch/mip at the preserved phase) steps the waveform and clicks.
        // Instead: choke-fade the OLD note to silence over ~4 ms (it keeps
        // rendering at its old pitch during the fade) and park the new note.
        // The render loop starts the parked note from silence the instant the
        // fade hits zero → the join is 0→0, click-free.
        let fade_samples = ((self.sample_rate * 0.004) as u32).max(1);
        let v = &mut self.voices[steal_idx];
        v.choke_level = v.env.level();
        v.choke_total = fade_samples;
        v.choke_remaining = fade_samples;
        v.pending_key = key;
        v.pending_note_id = note_id;
        v.pending_velocity = velocity;
        // Stamp as newest so a further steal this block picks a different victim
        // rather than clobbering this parked note.
        v.age_stamp = stamp;
    }

    fn release_voice(&mut self, key_match: Match<u16>, note_id_match: Match<u32>) {
        for v in self.voices.iter_mut() {
            // A note parked behind a choke-fade has not reached `key` yet, so
            // it needs matching separately. Without this the NoteOff is eaten
            // by the note being faded out and the parked note never releases.
            if v.pending_key != NOTE_FREE
                && matches_key(key_match, v.pending_key)
                && matches_note_id(note_id_match, v.pending_note_id)
            {
                v.pending_released = true;
            }
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
                // Choking the old note must not hand its slot to a parked note
                // that the player never got to release.
                v.pending_key = NOTE_FREE;
                v.pending_released = false;
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
            // Pitch bend — 14-bit value (LSB | MSB << 7), 8192 = center,
            // scaled by Bend Range param into semitones.
            0xe0 => {
                let raw = (key as i32) | ((raw_velocity as i32) << 7);
                let centered = raw - 8192;
                let normalised = (centered as f32) / 8191.0;
                let range = self.shared.params[P_BEND_RANGE].load(Ordering::Relaxed);
                self.shared
                    .pitch_bend_st
                    .store(normalised * range, Ordering::Relaxed);
            }
            0xb0 if key == 123 => self.release_voice(Match::All, Match::All),
            0xb0 if key == 120 => self.choke_voice(Match::All, Match::All),
            // Expressive CC mapping.
            //   CC  1 ModWheel    → LFO Depth (instant wobble)
            //   CC 11 Expression  → Cutoff (log)
            //   CC 71 Resonance   → Resonance
            //   CC 74 Brightness  → WT Pos
            0xb0 => {
                let v = raw_velocity as f32 / 127.0;
                // Always raise CC#1 into the mod_wheel source atomic so the
                // matrix sees it regardless of the legacy CC→LFO Depth route.
                if key == 1 {
                    self.shared.mod_wheel.store(v, Ordering::Relaxed);
                }
                // CC7 (Channel Volume) → output VCA, independent of any
                // learned/legacy CC route. Neutral (1.0) until it arrives.
                if key == 7 {
                    self.shared.cc_volume.store(v, Ordering::Relaxed);
                }
                let lin = |idx: usize, frac: f32| {
                    let def = &PARAMS[idx];
                    let val = def.min as f32 + frac * (def.max - def.min) as f32;
                    self.shared.params[idx].store(val, Ordering::Relaxed);
                };
                let log_cutoff = |frac: f32| {
                    let def = &PARAMS[P_CUTOFF];
                    let lo = (def.min as f32).ln();
                    let hi = (def.max as f32).ln();
                    let hz = (lo + frac * (hi - lo)).exp();
                    self.shared.params[P_CUTOFF].store(hz, Ordering::Relaxed);
                };
                if let Some(idx) = self.shared.midi_learn.handle_cc(key) {
                    lin(idx, v);
                } else {
                    match key {
                        1 => lin(P_LFO_DEPTH, v),
                        11 => log_cutoff(v),
                        71 => lin(P_RESONANCE, v),
                        74 => lin(P_WT_POS, v),
                        _ => {}
                    }
                }
            }
            // Channel aftertouch (status 0xD0) — single-byte pressure
            // value in data[1]. Map to LFO Depth for live timbral
            // pressure when keyboard supports it.
            0xd0 => {
                let pressure = key as f32 / 127.0;
                self.shared.aftertouch.store(pressure, Ordering::Relaxed);
                let def = &PARAMS[P_LFO_DEPTH];
                let val = def.min as f32 + pressure * (def.max - def.min) as f32;
                self.shared.params[P_LFO_DEPTH].store(val, Ordering::Relaxed);
            }
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
            CoreEventSpace::Transport(t) => {
                self.shared.host_bpm.store(t.tempo as f32, Ordering::Relaxed);
            }
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

        // Mod matrix slot snapshot — read once per block. Source/dest enums
        // are decoded out of the integer params; amount is straight float.
        let mod_slots: [osc::ModSlot; 2] = [
            osc::ModSlot {
                src: osc::ModSource::from_index(
                    self.shared.params[P_MOD1_SRC].load(Ordering::Relaxed) as u32),
                dst: osc::ModDest::from_index(
                    self.shared.params[P_MOD1_DST].load(Ordering::Relaxed) as u32),
                amt: self.shared.params[P_MOD1_AMT].load(Ordering::Relaxed),
            },
            osc::ModSlot {
                src: osc::ModSource::from_index(
                    self.shared.params[P_MOD2_SRC].load(Ordering::Relaxed) as u32),
                dst: osc::ModDest::from_index(
                    self.shared.params[P_MOD2_DST].load(Ordering::Relaxed) as u32),
                amt: self.shared.params[P_MOD2_AMT].load(Ordering::Relaxed),
            },
        ];
        let mod_wheel = self.shared.mod_wheel.load(Ordering::Relaxed);
        let aftertouch = self.shared.aftertouch.load(Ordering::Relaxed);
        let cc_vol_target = self.shared.cc_volume.load(Ordering::Relaxed);

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

            let adsr_p = AdsrParams::adsr(sr, attack_s, decay_s, sustain, release_s);

            let mut mix_l = 0.0_f32;
            let mut mix_r = 0.0_f32;
            // Advance the crossfade once per output sample, before the voice
            // loop. It used to sit inside that loop, after the `continue` that
            // skips idle voices — so it ticked once per SOUNDING voice and a
            // wavetable swap under an eight-note chord finished eight times
            // sooner than under a single note. The fade length is supposed to
            // be what hides the swap; it cannot depend on how many keys are down.
            self.fade_pos = (self.fade_pos + self.fade_inc).min(1.0);
            let fade_pos = self.fade_pos;
            for v in self.voices.iter_mut() {
                if v.key == NOTE_FREE && v.env.is_idle() && v.choke_remaining == 0 {
                    continue;
                }

                let base_hz =
                    midi_note_to_hz(v.key as f32 + self.shared.pitch_bend_st.load(Ordering::Relaxed));
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
                    fenv: AdsrParams::adsr(sr, fenv_a, fenv_d, fenv_s, fenv_r),
                    lfo_shape,
                    lfo_dest,
                    lfo_rate_hz,
                    lfo_depth,
                    frames: &self.frames,
                    frame_a_prev: &self.frame_a_prev,
                    frame_a_fade: fade_pos,
                    sync_on: self.shared.params[P_SYNC].load(Ordering::Relaxed) >= 0.5,
                    sync_ratio: self.shared.params[P_SYNC_RATIO].load(Ordering::Relaxed),
                    fm_ratio: self.shared.params[P_FM_RATIO].load(Ordering::Relaxed),
                    fm_amount: self.shared.params[P_FM_AMT].load(Ordering::Relaxed),
                    mod_slots,
                    mod_wheel,
                    aftertouch,
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
                        if v.pending_key != NOTE_FREE {
                            // Deferred steal fade done — start the parked note
                            // from silence (env attacks from 0 → no click).
                            v.key = v.pending_key;
                            v.note_id = v.pending_note_id;
                            v.velocity = v.pending_velocity;
                            v.pending_key = NOTE_FREE;
                            v.env = AdsrEnvelope::default();
                            v.env.gate_on();
                            v.filter_env = AdsrEnvelope::default();
                            v.filter_env.gate_on();
                            v.lfo_phase = 0.0;
                            v.configure_unison(unison, detune);
                            v.scatter_phases();
                            // Released while parked: start it, then let it go
                            // straight into release, so a very short note
                            // sounds short instead of sounding forever.
                            if v.pending_released {
                                v.env.gate_off();
                                v.filter_env.gate_off();
                                v.pending_released = false;
                            }
                        } else {
                            v.env = AdsrEnvelope::default();
                            v.key = NOTE_FREE;
                        }
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
            let cc_vol = self.smooth_cc_vol.step(cc_vol_target, sr);
            let final_l = mix_l * voice_scale * out_lin * cc_vol;
            let final_r = mix_r * voice_scale * out_lin * cc_vol;
            out_l[i] = final_l;
            out_r[i] = final_r;
            self.shared.scope.push((final_l + final_r) * 0.5);
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
            // try_lock, never lock: the writer may be holding this while it
            // bakes the next pyramid, and waiting would block the callback on
            // a normal-priority thread. Re-arm and pick it up next block.
            let Some(mut slot) = self.shared.pending_frames.try_lock() else {
                self.shared.pending_swap.store(true, Ordering::Release);
                return;
            };
            let Some(new_frames) = slot.take() else { return };
            drop(slot);
            if new_frames.is_empty() {
                return;
            }
            // Cheap "did frames[0] actually move" check via the
            // level-0 Arc pointer — saves an unnecessary crossfade
            // when later frames changed but the head didn't.
            let head_moved = self
                .frames
                .first()
                .map(|cur| !std::sync::Arc::ptr_eq(&new_frames[0].levels[0], &cur.levels[0]))
                .unwrap_or(true);
            if head_moved {
                self.frame_a_prev = self
                    .frames
                    .first()
                    .cloned()
                    .unwrap_or_else(|| new_frames[0].clone());
                self.fade_pos = 0.0;
            }
            self.frames = new_frames;
            self.shared.pending_preset.store(u32::MAX, Ordering::Relaxed);
        }
    }
}

impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, PluginShared, PluginMainThread<'a>>
    for PluginAudioProcessor<'a>
{
    fn activate(
        host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut PluginMainThread<'a>,
        shared: &'a PluginShared,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        let frames: Vec<MipWavetable> = {
            let guard = shared.wavetable.lock();
            guard.clone()
        };
        // 12 ms crossfade between old/new frames[0] — short enough to
        // feel immediate, long enough to bury the curve-edit zipper noise.
        let fade_samples = (sr * 0.012).max(1.0);
        let fade_inc = 1.0 / fade_samples;
        let frame_a_prev = frames
            .first()
            .cloned()
            .expect("wavetable must have at least one frame");
        Ok(Self {
            shared,
            host,
            voices: std::array::from_fn(|_| WaveVoice::default()),
            frames,
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
            smooth_cc_vol: SmoothedParam::new(1.0),
            sample_rate: sr,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // Flush denormals to zero — long decays / feedback loops
        // otherwise generate ≈10⁻³⁸ floats that murder CPU and cause
        // periodic ticks at the buffer rate. RAII restores host CSR.
        let _denormals = superduper_dsp_sdk::denormals::Guard::new();
        // Flush GUI-driven param changes back to the host so REAPER can
        // record the move into the automation lane.
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(
            &self.shared.params, &self.shared.dirty_params, events.output);
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(
            &self.shared.gesture_begin, &self.shared.gesture_end, events.output);
        self.maybe_swap_wavetable();
        // Preset selector: if the host or producer-pal/MCP moved the Preset
        // param, wake the main thread to recall it — apply_preset allocates
        // mip tables, which is forbidden on the audio thread.
        if superduper_dsp_sdk::clap_helpers::preset_recall_target(
            self.shared.params[P_PRESET].load(Ordering::Relaxed),
            &self.shared.active_preset,
            PRESETS.len(),
        )
        .is_some()
        {
            self.host.shared().request_callback();
        }

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
                let lfo_rate = {
                    let sync = self.shared.params[P_LFO_SYNC]
                        .load(Ordering::Relaxed)
                        >= 0.5;
                    if sync {
                        let div = self.shared.params[P_LFO_DIV]
                            .load(Ordering::Relaxed) as u32;
                        let bpm = self.shared.host_bpm.load(Ordering::Relaxed);
                        superduper_synth_core::dsp_blocks::sync_division_hz(div, bpm)
                    } else {
                        self.shared.params[P_LFO_RATE].load(Ordering::Relaxed)
                    }
                };
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
        ParamDef::write_info_stepped(PARAMS, idx, info, STEPPED_PARAMS);
    }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        let i = id.get() as usize;
        self.shared.params.get(i).map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(&mut self, id: ClapId, v: f64, w: &mut ParamDisplayWriter) -> core::fmt::Result {
        use core::fmt::Write;
        let pid = id.get() as usize;
        // Custom-label enum-style params so the DAW shows readable names
        // instead of "0.00" / "3.00".
        if pid == P_MOD1_SRC || pid == P_MOD2_SRC {
            let name = match v.round() as i32 {
                1 => "LFO",
                2 => "Velocity",
                3 => "ModWheel",
                4 => "Aftertouch",
                5 => "FilterEnv",
                _ => "None",
            };
            return write!(w, "{}", name);
        }
        if pid == P_SYNC {
            return write!(w, "{}", if v >= 0.5 { "On" } else { "Off" });
        }
        if pid == P_MOD1_DST || pid == P_MOD2_DST {
            let name = match v.round() as i32 {
                1 => "Cutoff",
                2 => "Pitch",
                3 => "WT Pos",
                4 => "Resonance",
                5 => "Drive",
                6 => "Volume",
                _ => "None",
            };
            return write!(w, "{}", name);
        }
        if pid == P_PRESET {
            if let Some(r) = superduper_dsp_sdk::clap_helpers::preset_value_to_text(
                |i| PRESETS.get(i).map(|p| p.name),
                v,
                w,
            ) {
                return r;
            }
        }
        ParamDef::write_display(PARAMS, id, v, w)
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        // Let "Reese Bass" / "808 Sub" etc. resolve to the preset index too.
        if id.get() as usize == P_PRESET {
            if let Some(v) = superduper_dsp_sdk::clap_helpers::preset_text_to_value(
                PRESETS.len(),
                |i| PRESETS.get(i).map(|p| p.name),
                t,
            ) {
                return Some(v);
            }
        }
        ParamDef::parse_text(PARAMS, id, t)
    }
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
        // Preset selector recall (main thread → apply_preset's alloc is legal).
        if let Some(idx) = superduper_dsp_sdk::clap_helpers::preset_recall_target(
            self.shared.params[P_PRESET].load(Ordering::Relaxed),
            &self.shared.active_preset,
            PRESETS.len(),
        ) {
            apply_preset(&self.shared.inner, idx);
        }
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
    }
}

// ---------------------------------------------------------------------------
// CLAP state — saves params + the user-drawn frame_a so a project reload
// brings back exactly what the user designed (without this REAPER loses
// the curve and the active preset).
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize)]
struct WaveState {
    version: u32,
    params: Vec<f32>,
    bypass: bool,
    /// Legacy single-frame field — kept so v1 builds can still load
    /// projects saved by newer builds. Always populated from frames[0].
    frame_a: Vec<f32>,
    /// Full N-frame wavetable. Empty for v1-format saves.
    #[serde(default)]
    frames: Vec<Vec<f32>>,
    /// Active preset index — informational, helps the GUI restore the
    /// dropdown selection.
    active_preset: u32,
}

const WAVE_STATE_VERSION: u32 = 1;

impl PluginStateImpl for PluginMainThread<'_> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        // Snapshot every active frame so the full N-frame wavetable
        // round-trips through project save/load. frame_a stays as
        // the canonical frames[0] for backward compat with v1 readers.
        let frames: Vec<Vec<f32>> = {
            let guard = self.shared.wavetable.lock();
            guard
                .iter()
                .map(|mip| mip.levels[0].iter().copied().collect())
                .collect()
        };
        let frame_a = frames.first().cloned().unwrap_or_default();
        let state = WaveState {
            version: WAVE_STATE_VERSION,
            params: self.shared.params.iter().map(|a| a.load(Ordering::Relaxed)).collect(),
            bypass: self.shared.bypass.load(Ordering::Relaxed),
            frame_a,
            frames,
            active_preset: self.shared.active_preset.load(Ordering::Relaxed),
        };
        serde_json::to_writer(output, &state)
            .map_err(|_| PluginError::Message("state JSON write error"))
    }

    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        let state: WaveState = serde_json::from_reader(input)
            .map_err(|_| PluginError::Message("state JSON read error"))?;
        if state.version != WAVE_STATE_VERSION {
            return Err(PluginError::Message("state version mismatch"));
        }
        for (i, v) in state.params.iter().enumerate() {
            if let Some(slot) = self.shared.params.get(i) {
                slot.store(*v, Ordering::Relaxed);
            }
        }
        self.shared.bypass.store(state.bypass, Ordering::Relaxed);
        // Prefer the full `frames` array; fall back to v1's single
        // `frame_a` if absent. Every entry must be exactly WT_SIZE.
        let mips: Vec<MipWavetable> = if !state.frames.is_empty() {
            state.frames
                .iter()
                .filter(|f| f.len() == osc::WT_SIZE)
                .map(|f| osc::mip_from_table(f))
                .collect()
        } else if state.frame_a.len() == osc::WT_SIZE {
            vec![osc::mip_from_table(&state.frame_a)]
        } else {
            Vec::new()
        };
        if !mips.is_empty() {
            push_custom_frames(&self.shared, mips);
        }
        self.shared.active_preset.store(state.active_preset, Ordering::Relaxed);
        Ok(())
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
            .register::<PluginState>()
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
        let shared = PluginShared::new();
        // Try the auto-saved "last edited" snapshot — becomes the default
        // for a fresh plugin instance. PluginStateImpl::load runs AFTER
        // new_shared so any project state will override these values.
        //
        // `SUPERDUPER_WAVE_FACTORY=1` skips this so the plugin boots on the
        // factory Init (Sine) wavetable regardless of what the user last drew.
        // Tests set it so DSP-quality assertions don't depend on ~/.superduper-dsp.
        let factory_only = std::env::var_os("SUPERDUPER_WAVE_FACTORY").is_some();
        if let Some(preset) = (!factory_only)
            .then(|| user_extra::repo().load_last(PARAMS.len()))
            .flatten()
        {
            use std::sync::atomic::Ordering;
            for (i, v) in preset.params.iter().enumerate() {
                if let Some(slot) = shared.params.get(i) {
                    slot.store(*v, Ordering::Relaxed);
                }
            }
            // Restore all N frames (auto-default carries the same
            // count as when the user last saved — could be 1..=16).
            let frames = preset.extra.effective_frames();
            let mips: Vec<_> = frames.iter().map(|f| osc::mip_from_table(f)).collect();
            push_custom_frames(&shared, mips);
        }
        Ok(shared)
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
