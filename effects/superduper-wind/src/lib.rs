//! SuperDuper Wind — a wind/breath instrument (kurai / nay / low Bashkir
//! flute) built on Spectral Modeling Synthesis: a deterministic additive
//! tone plus a stochastic, formant-bandpassed breath-noise layer.
//!
//! One plugin, two `Mode`s:
//! - **Instrument** — an 8-voice polyphonic note-driven synth. DSP lives in
//!   `voice.rs` (`WindVoice`).
//! - **Overlay** — an audio-in effect that reads the main input, tracks its
//!   envelope (and loosely its pitch), and adds the SAME breath layer on
//!   top — "sidechain breath" for any lead/vocal already on the track.
//!
//! CLAP plumbing mirrors `superduper-vocoder` (audio port + note port on
//! the same plugin) with one simplification: Wind needs only ONE stereo
//! port (in-place paired) rather than vocoder's separate sidechain port,
//! because Overlay reads/writes the same main port effects normally use.

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod howl;
pub mod noise;
pub mod presets;
pub mod voice;

pub use howl::HowlEngine;
pub use noise::{ColorNoise, WobbleGen};
pub use presets::{Preset, DEFAULT_PRESET, PRESETS, PRESET_COUNT};
pub use voice::{WindParams, WindVoice, N_HARM, NOTE_FREE};

use atomic_float::AtomicF32;
use clack_common::events::spaces::CoreEventSpace;
use clack_common::events::Match;
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
use clack_extensions::state::PluginState;

use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use superduper_dsp_sdk::clap_helpers;
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{
    AdsrEnvelope, AdsrParams, Biquad, EnvelopeDetector, midi_note_to_hz,
};
use superduper_synth_core::pitch::YinPitchTracker;

fn init_logging() {
    superduper_dsp_sdk::log::init("wind");
}
use superduper_dsp_sdk::slog;

// ===========================================================================
// Parameter table — FROZEN once shipped (REAPER caches the layout per slot).
// ===========================================================================

pub const PARAMS: &[ParamDef] = &[
    // 0 = Instrument (note-driven poly synth), 1 = Overlay (audio-in breath layer).
    ParamDef { id: 0,  name: b"Mode",       min: 0.0,    max: 1.0,     default: 0.0,    unit: ""   },
    ParamDef { id: 1,  name: b"Breath",     min: 0.0,    max: 1.0,     default: 0.5,    unit: ""   },
    ParamDef { id: 2,  name: b"Jitter",     min: 0.0,    max: 1.0,     default: 0.15,   unit: ""   },
    ParamDef { id: 3,  name: b"Shimmer",    min: 0.0,    max: 1.0,     default: 0.15,   unit: ""   },
    ParamDef { id: 4,  name: b"Tone",       min: 0.0,    max: 1.0,     default: 0.4,    unit: ""   },
    ParamDef { id: 5,  name: b"Formant",    min: -12.0,  max: 12.0,    default: 0.0,    unit: "st" },
    ParamDef { id: 6,  name: b"Attack",     min: 1.0,    max: 3000.0,  default: 90.0,   unit: "ms" },
    ParamDef { id: 7,  name: b"Release",    min: 1.0,    max: 4000.0,  default: 350.0,  unit: "ms" },
    ParamDef { id: 8,  name: b"Chiff",      min: 0.0,    max: 1.0,     default: 0.25,   unit: ""   },
    // 0 = pink/dark/wind-like, 1 = white/airy.
    ParamDef { id: 9,  name: b"Color",      min: 0.0,    max: 1.0,     default: 0.4,    unit: ""   },
    // Repurposed from the old static "Cutoff" lowpass — Howl is the
    // Farnell howling-wind engine's intensity: 0 = broad/gentle bandpasses
    // barely swept (mostly the old gentle breath character), 1 = tight
    // high-Q resonant bands sweeping widely (dominant howling wind). Also
    // fades the additive tone down as it rises — see `voice.rs`.
    ParamDef { id: 10, name: b"Howl",       min: 0.0,    max: 1.0,     default: 0.2,    unit: ""   },
    ParamDef { id: 11, name: b"Mix",        min: 0.0,    max: 1.0,     default: 0.5,    unit: ""   },
    ParamDef { id: 12, name: b"Output",     min: -24.0,  max: 24.0,    default: 0.0,    unit: "dB" },
    ParamDef { id: 13, name: b"Bend Range", min: 0.0,    max: 24.0,    default: 2.0,    unit: "ST" },
    // NEW — gust surges: rate (mapped 0.05-0.5 Hz) AND depth in one knob.
    // Drives a single shared envelope (`gust_gen` in the audio processor)
    // that swells the whole noise "bed" amplitude uniformly across every
    // voice (Instrument) or the wind-bed + input-ducking filter (Overlay) —
    // a real gust doesn't hit polyphonic notes independently.
    ParamDef { id: 14, name: b"Gust",       min: 0.0,    max: 1.0,     default: 0.3,    unit: ""   },
    // NEW — Aeolian-tone (vortex-shedding) whistle blend: 0 = pure
    // broadband howl, 1 = strong tonal whistle riding on top, gliding in
    // pitch+amplitude with Gust (Strouhal relation, see `howl.rs`). Gated
    // by `Howl` — no whistle in the gentle-breath end of the range.
    // Appended just before Preset, per the "PARAMS order frozen, append at
    // the end" rule — Preset shifts from 15 to 16.
    ParamDef { id: 15, name: b"Whistle",    min: 0.0,    max: 1.0,     default: 0.0,    unit: ""   },
    // Preset selector — stepped 0..N-1, always LAST (see CLAUDE.md's
    // cross-cutting "Preset-selector param" section: recall plumbing via
    // sdk::clap_helpers::preset_recall_target + apply_preset on the main
    // thread). PRESET_COUNT is a plain const (NOT `PRESETS.len()`) to dodge
    // the const-eval cycle (E0391) noted there.
    ParamDef { id: 16, name: b"Preset",     min: 0.0,    max: (presets::PRESET_COUNT - 1) as f64, default: presets::DEFAULT_PRESET as f64, unit: "" },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[0, 16];

pub const P_MODE: usize = 0;
pub const P_BREATH: usize = 1;
pub const P_JITTER: usize = 2;
pub const P_SHIMMER: usize = 3;
pub const P_TONE: usize = 4;
pub const P_FORMANT: usize = 5;
pub const P_ATTACK: usize = 6;
pub const P_RELEASE: usize = 7;
pub const P_CHIFF: usize = 8;
pub const P_COLOR: usize = 9;
pub const P_HOWL: usize = 10;
pub const P_MIX: usize = 11;
pub const P_OUTPUT: usize = 12;
pub const P_BEND_RANGE: usize = 13;
pub const P_GUST: usize = 14;
pub const P_WHISTLE: usize = 15;
pub const P_PRESET: usize = 16;

pub const VOICE_COUNT: usize = 8;

const MODE_OVERLAY_THRESHOLD: f32 = 0.5;
const DECAY_S: f32 = 0.03;
const SUSTAIN: f32 = 1.0;
const VOICE_SCALE: f32 = 0.55;

/// Base formant centres/bandwidths/gains at Formant = 0 st — dark, low,
/// kurai-like by default. The `Formant` param multiplies these (2^(st/12))
/// rather than exposing F1/F2/F3 directly, keeping the fixed param table
/// small while presets still land on very different formant colours.
const BASE_FORMANT_F: [f32; 3] = [500.0, 1100.0, 2000.0];
const BASE_FORMANT_BW: [f32; 3] = [180.0, 260.0, 340.0];
const BASE_FORMANT_GAIN: [f32; 3] = [1.0, 0.85, 0.65];

/// Additive-harmonic brightness curve: harmonic `n` (0-based) gets
/// `(n+1)^-rolloff`. Tone=0 → steep rolloff (mostly fundamental, dark);
/// Tone=1 → gentle rolloff (rich in overtones, bright).
fn harmonics_from_tone(tone: f32) -> [f32; N_HARM] {
    let rolloff = 2.6 - 2.1 * tone.clamp(0.0, 1.0);
    std::array::from_fn(|n| ((n + 1) as f32).powf(-rolloff))
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

// ===========================================================================
// Shared params (Arc so the egui thread can clone a handle).
// ===========================================================================

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: AtomicBool,
    pub dirty_params: [AtomicBool; PARAMS.len()],
    pub gesture_begin: [AtomicBool; PARAMS.len()],
    pub gesture_end: [AtomicBool; PARAMS.len()],
    pub active_preset: AtomicU32,
    /// Live MIDI pitch-bend in semitones (signed), scaled by `Bend Range`.
    pub pitch_bend_st: AtomicF32,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
}

pub struct PluginShared {
    pub inner: SharedParams,
}

impl PluginShared {
    pub fn new() -> Self {
        // Boot straight into the default preset (Kurai) so a freshly
        // inserted plugin already sounds like a wind instrument.
        let init = &PRESETS[DEFAULT_PRESET];
        let params: [AtomicF32; PARAMS.len()] =
            std::array::from_fn(|i| AtomicF32::new(init.values[i]));
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params,
                bypass: AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| AtomicBool::new(false)),
                active_preset: AtomicU32::new(DEFAULT_PRESET as u32),
                pitch_bend_st: AtomicF32::new(0.0),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
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

/// Recall preset `idx`: writes every param, marks every param dirty (so the
/// host/automation lane sees the switch — lesson 21d), and reflects the
/// selection back into both the `Preset` CLAP param and `active_preset`.
/// Only ever called from the main thread (on_main_thread / params::flush) —
/// see `sdk::clap_helpers::preset_recall_target` for the RT-safety contract.
pub fn apply_preset_idx(shared: &SharedParamsInner, idx: usize) {
    let Some(preset) = PRESETS.get(idx) else { return };
    for (i, v) in preset.values.iter().enumerate() {
        if let Some(atom) = shared.params.get(i) {
            atom.store(*v, Ordering::Relaxed);
        }
    }
    for flag in shared.dirty_params.iter() {
        flag.store(true, Ordering::Relaxed);
    }
    if let Some(atom) = shared.params.get(P_PRESET) {
        atom.store(idx as f32, Ordering::Relaxed);
    }
    shared.active_preset.store(idx as u32, Ordering::Relaxed);
}

/// GUI helper — write into a param atomic and raise its dirty flag so the
/// audio thread emits a `ParamValueEvent` (REAPER automation capture).
pub fn write_param(shared: &SharedParamsInner, idx: usize, value: f32) {
    if let Some(atom) = shared.params.get(idx) {
        atom.store(value, Ordering::Relaxed);
        if let Some(flag) = shared.dirty_params.get(idx) {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

// ---------------------------------------------------------------------------
// Main-thread + audio processor
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {
    /// Host main-thread callback — the audio thread calls request_callback()
    /// when the Preset param moved; the (allocating) recall runs here.
    fn on_main_thread(&mut self) {
        if let Some(idx) = superduper_dsp_sdk::clap_helpers::preset_recall_target(
            self.shared.params[P_PRESET].load(Ordering::Relaxed),
            &self.shared.active_preset,
            PRESETS.len(),
        ) {
            apply_preset_idx(&self.shared.inner, idx);
        }
    }
}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    /// Host handle — used to wake the main thread (request_callback) for
    /// the (allocating) Preset recall.
    host: HostAudioProcessorHandle<'a>,
    sample_rate: f32,

    // ---- Instrument mode ----
    voices: [WindVoice; VOICE_COUNT],
    next_age: u64,

    // ---- Overlay mode ----
    /// The same Farnell howling-wind engine Instrument voices use, driving
    /// Overlay's wind-bed (not per-voice — Overlay has no notes).
    overlay_howl: HowlEngine,
    overlay_env: EnvelopeDetector,
    overlay_pitch: YinPitchTracker,
    /// Slow resonant lowpass that the shared gust envelope opens/closes on
    /// the DRY input in Overlay mode — "the wind blows through it".
    duck_l: Biquad,
    duck_r: Biquad,

    // ---- Shared gust engine ----
    /// One shared gust-surge generator — a real gust swells the whole wind
    /// bed (and, in Overlay, ducks the input) uniformly; it does NOT live
    /// per-voice. Reuses `WobbleGen` (slow smoothed-noise wander) at a
    /// 0.05-0.5 Hz rate set by the `Gust` param.
    gust_gen: WobbleGen,

    // Pre-allocated scratch — `activate()` is the only place we're allowed
    // to touch the allocator (lesson 11).
    in_l: Box<[f32]>,
    in_r: Box<[f32]>,
    /// Per-sample gust curves, filled once per block in `process` and shared by
    /// the Instrument and Overlay paths (pre-allocated: no heap on the audio
    /// thread).
    gust_curve: Box<[f32]>,
    swell_curve: Box<[f32]>,
    out_l_scratch: Box<[f32]>,
    out_r_scratch: Box<[f32]>,
}

/// CLAP note matching: `Match::All` (the wildcard a host sends for
/// all-notes-off) matches every voice, `Specific` compares the value.
fn matches_key(m: Match<u16>, key: u8) -> bool {
    match m {
        Match::All => true,
        Match::Specific(k) => k as u8 == key,
    }
}

fn matches_note_id(m: Match<u32>, note_id: i32) -> bool {
    match m {
        Match::All => true,
        Match::Specific(id) => id as i32 == note_id,
    }
}

impl<'a> PluginAudioProcessor<'a> {
    fn allocate_voice(&mut self, key: u8, velocity: f32, note_id: i32) {
        self.next_age = self.next_age.wrapping_add(1);
        let stamp = self.next_age;
        let sr = self.sample_rate;
        // 1. Legato retrigger of an already-sounding key — resume from the
        // current envelope level, don't rescatter phases (would click).
        for v in self.voices.iter_mut() {
            if v.key == key && v.note_id == note_id {
                v.env.retrigger();
                v.velocity = velocity;
                v.age_stamp = stamp;
                v.choke_remaining = 0;
                return;
            }
        }
        // 2. Free slot.
        if let Some(v) = self
            .voices
            .iter_mut()
            .find(|v| v.env.is_idle() && v.choke_remaining == 0)
        {
            v.key = key;
            v.note_id = note_id;
            v.velocity = velocity;
            v.age_stamp = stamp;
            v.env = AdsrEnvelope::default();
            v.env.gate_on();
            v.choke_remaining = 0;
            v.on_note_on(sr);
            return;
        }
        // 3. Steal — quietest releasing voice, else oldest. Skip voices
        // already mid-choke-fade (lesson 17b — re-choking clicks + clobbers
        // the note parked on them).
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
                let mut oldest = u64::MAX;
                for (i, v) in self.voices.iter().enumerate() {
                    if v.age_stamp < oldest {
                        oldest = v.age_stamp;
                        steal_idx = i;
                    }
                }
            }
        }
        // Deferred steal — choke-fade the OLD note to silence (~4 ms), park
        // the new note; the render loop starts it from silence when the
        // fade ends so the join is click-free.
        let fade_samples = ((sr * 0.004) as u32).max(1);
        let v = &mut self.voices[steal_idx];
        v.choke_level = v.env.level();
        v.choke_total = fade_samples;
        v.choke_remaining = fade_samples;
        v.pending_key = key;
        v.pending_note_id = note_id;
        v.pending_velocity = velocity;
        v.age_stamp = stamp;
    }

    fn release_voice(&mut self, key_match: Match<u16>, note_id_match: Match<u32>) {
        for v in self.voices.iter_mut() {
            // A note parked behind a choke-fade hasn't reached `key` yet, so it
            // has to be matched separately — all events are drained at block
            // start, so a short note can easily be released before its fade ends.
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
                // A choke means "stop now", so a note still parked behind the
                // fade must be discarded rather than promoted after it.
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
                    self.allocate_voice(key, raw_velocity as f32 / 127.0, -1);
                }
            }
            0x80 => self.release_voice(Match::Specific(key as u16), Match::All),
            // Pitch bend — 14-bit value (LSB | MSB<<7), 8192 = centre.
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
                let note_id = match n.note_id() {
                    Match::Specific(id) => id as i32,
                    Match::All => -1,
                };
                self.allocate_voice(key, n.velocity().clamp(0.0, 1.0) as f32, note_id);
            }
            CoreEventSpace::NoteOff(n) => self.release_voice(n.key(), n.note_id()),
            CoreEventSpace::NoteChoke(n) => self.choke_voice(n.key(), n.note_id()),
            CoreEventSpace::Midi(m) => self.handle_midi_event(m.data()),
            _ => {}
        }
    }

    /// Walk the event stream for note on/off/choke (CLAP + raw MIDI, both
    /// dialects — lesson 14). Never touches the param dirty flags (CC would,
    /// but Wind maps no CC — lesson 21b doesn't apply here, just avoided).
    fn handle_note_events(&mut self, events: &InputEvents) {
        for event in events {
            let Some(core) = event.as_core_event() else {
                continue;
            };
            self.handle_note_event(&core);
        }
    }
}

/// Render the Instrument voice pool for one block. A free function (not a
/// `&mut self` method) so the caller can pass disjoint `self` fields —
/// `self.voices` mutably alongside `self.shared` — without the borrow
/// checker treating the whole struct as one opaque borrow.
#[allow(clippy::too_many_arguments)]
fn render_instrument(
    voices: &mut [WindVoice; VOICE_COUNT],
    out_l: &mut [f32],
    out_r: &mut [f32],
    scope: &superduper_synth_core::gui::LiveScope,
    sr: f32,
    bend_st: f32,
    harmonics: [f32; N_HARM],
    formant_f: [f32; 3],
    breath: f32,
    jitter: f32,
    shimmer: f32,
    chiff: f32,
    color: f32,
    howl: f32,
    // Per-sample gust amplitude multiplier — see the note in `process`.
    gust: &[f32],
    whistle: f32,
    attack_s: f32,
    release_s: f32,
    output_db: f32,
) {
    let adsr = AdsrParams::adsr(sr, attack_s, DECAY_S, SUSTAIN, release_s);
    let out_lin = 10f32.powf(output_db / 20.0);
    for i in 0..out_l.len() {
        let mut mix_l = 0.0_f32;
        let mut mix_r = 0.0_f32;
        let gust_mult = gust.get(i).copied().unwrap_or(1.0);
        for v in voices.iter_mut() {
            if v.key == NOTE_FREE && v.env.is_idle() && v.choke_remaining == 0 {
                continue;
            }
            let root = midi_note_to_hz(v.key as f32 + bend_st);
            let wp = WindParams {
                sr,
                root_hz: root,
                harmonics,
                formant_f,
                formant_bw: BASE_FORMANT_BW,
                formant_gain: BASE_FORMANT_GAIN,
                breath,
                jitter,
                shimmer,
                chiff,
                color,
                howl,
                gust_mult,
                whistle,
            };
            if v.choke_remaining > 0 {
                let fade = v.choke_remaining as f32 / v.choke_total as f32;
                let (l, r) = v.process(&wp);
                let amp = fade * v.choke_level * v.velocity;
                mix_l += l * amp;
                mix_r += r * amp;
                v.choke_remaining -= 1;
                if v.choke_remaining == 0 {
                    if v.pending_key != NOTE_FREE {
                        v.key = v.pending_key;
                        v.note_id = v.pending_note_id;
                        v.velocity = v.pending_velocity;
                        v.pending_key = NOTE_FREE;
                        v.env = AdsrEnvelope::default();
                        v.env.gate_on();
                        v.on_note_on(sr);
                        // Released while parked: start it and let it go straight
                        // into release, so a very short note sounds short instead
                        // of sounding forever.
                        if v.pending_released {
                            v.env.gate_off();
                            v.pending_released = false;
                        }
                    } else {
                        v.env = AdsrEnvelope::default();
                        v.key = NOTE_FREE;
                    }
                }
                continue;
            }
            let env = v.env.process(adsr);
            if env <= 1e-5 && v.env.is_idle() {
                v.key = NOTE_FREE;
                continue;
            }
            let (l, r) = v.process(&wp);
            let amp = env * v.velocity;
            mix_l += l * amp;
            mix_r += r * amp;
        }
        let l = mix_l * VOICE_SCALE * out_lin;
        let r = mix_r * VOICE_SCALE * out_lin;
        out_l[i] = l;
        out_r[i] = r;
        scope.push((l + r) * 0.5);
    }
}

/// Duck filter fully-open cutoff (Hz) — effectively transparent, and the
/// fully-closed floor (Hz) reached at max Gust*swell — a deliberately deep,
/// obviously-audible muffle so "the wind blows through it" reads clearly.
const DUCK_OPEN_HZ: f32 = 17_000.0;
const DUCK_CLOSED_HZ: f32 = 500.0;

/// Render Overlay mode for one block — same free-function shape as
/// `render_instrument`, for the same borrow-splitting reason. Two coupled
/// effects, both keyed to the input so an insert on another track is
/// unmistakably "wind is happening here":
///   1. A wind-bed (the shared `HowlEngine`) keyed to the input's envelope,
///      swelling further with the shared gust.
///   2. A slow resonant lowpass on the DRY input itself, closing on gust
///      peaks — a real sidechain-style interaction, not just an added layer.
#[allow(clippy::too_many_arguments)]
fn render_overlay(
    in_l: &[f32],
    in_r: &[f32],
    out_l: &mut [f32],
    out_r: &mut [f32],
    scope: &superduper_synth_core::gui::LiveScope,
    howl_engine: &mut HowlEngine,
    env_det: &mut EnvelopeDetector,
    pitch: &mut YinPitchTracker,
    duck_l: &mut Biquad,
    duck_r: &mut Biquad,
    sr: f32,
    breath: f32,
    color: f32,
    mix: f32,
    howl: f32,
    whistle: f32,
    gust_amt: f32,
    // Per-sample gust swell in 0..1 (shared with the Instrument path).
    swell: &[f32],
    // Per-sample amplitude multiplier derived from `swell`.
    gust: &[f32],
    output_db: f32,
) {
    let out_lin = 10f32.powf(output_db / 20.0);
    // The ducking filter is retuned once per block (a biquad retune per sample
    // would be wasteful and inaudible at gust rates) using the block's mean
    // swell, while the bed amplitude follows the curve sample by sample.
    let mean_swell = if swell.is_empty() {
        0.0
    } else {
        swell.iter().take(out_l.len()).sum::<f32>() / out_l.len().max(1) as f32
    };
    // Duck depth is the SAME swell driving the wind-bed amplitude, so the
    // bed getting louder and the track getting muffled are perceived as one
    // coherent gust event, not two uncorrelated processes.
    let duck_depth = gust_amt * mean_swell;
    let duck_cutoff_hz = lerp(DUCK_OPEN_HZ, DUCK_CLOSED_HZ, duck_depth);
    duck_l.set_lpf(sr, duck_cutoff_hz, 0.707);
    duck_r.set_lpf(sr, duck_cutoff_hz, 0.707);

    for i in 0..out_l.len() {
        let gust_mult = gust.get(i).copied().unwrap_or(1.0);
        let il = in_l[i];
        let ir = in_r[i];
        let mono = 0.5 * (il + ir);
        let env = env_det.process(mono, sr, 8.0, 140.0);
        pitch.push(mono);
        // Loosely follow the input's pitch — real vocal-tract-style
        // instruments do this a little; kept subtle so it reads as natural
        // rather than chipmunk-y.
        let f0 = pitch.current_hz();
        let pitch_mult = (f0 / 220.0).clamp(0.4, 2.5).powf(0.2);

        let (hl, hr) = howl_engine.process(sr, howl, color, pitch_mult, whistle, gust_mult);

        // "Keyed to the input envelope" — a small ambient floor even on
        // silence, swelling hard once the track is actually playing, times
        // the shared gust surge.
        let env_norm = (env * 3.0).clamp(0.0, 1.3);
        let bed_amp = breath * (0.3 + 0.7 * env_norm) * gust_mult;

        let dry_l = duck_l.process(il);
        let dry_r = duck_r.process(ir);

        let l = (dry_l + hl * bed_amp * mix) * out_lin;
        let r = (dry_r + hr * bed_amp * mix) * out_lin;
        out_l[i] = l;
        out_r[i] = r;
        scope.push((l + r) * 0.5);
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
        slog!("activate: sr={}", sr);
        let max_frames = audio_config.max_frames_count as usize;
        Ok(Self {
            shared,
            host,
            sample_rate: sr,
            voices: std::array::from_fn(WindVoice::new),
            next_age: 0,
            overlay_howl: HowlEngine::new(0x4A6D_5D1B),
            overlay_env: EnvelopeDetector::default(),
            overlay_pitch: YinPitchTracker::new(sr, 60.0, 1200.0, 1024, 256, 220.0),
            duck_l: Biquad::default(),
            duck_r: Biquad::default(),
            gust_gen: WobbleGen::new(0x7F4A_7C15),
            in_l: vec![0.0; max_frames].into_boxed_slice(),
            in_r: vec![0.0; max_frames].into_boxed_slice(),
            gust_curve: vec![1.0; max_frames].into_boxed_slice(),
            swell_curve: vec![0.0; max_frames].into_boxed_slice(),
            out_l_scratch: vec![0.0; max_frames].into_boxed_slice(),
            out_r_scratch: vec![0.0; max_frames].into_boxed_slice(),
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // Flush denormals to zero — formant biquads + noise-filter chains
        // otherwise spin up ~10⁻³⁸ floats that murder CPU on long releases.
        let _denormals = superduper_dsp_sdk::denormals::Guard::new();

        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(
            &self.shared.params,
            &self.shared.dirty_params,
            events.output,
        );
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(
            &self.shared.gesture_begin,
            &self.shared.gesture_end,
            events.output,
        );
        if superduper_dsp_sdk::clap_helpers::preset_recall_target(
            self.shared.params[P_PRESET].load(Ordering::Relaxed),
            &self.shared.active_preset,
            PRESETS.len(),
        )
        .is_some()
        {
            self.host.shared().request_callback();
        }

        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, events.input);
        self.handle_note_events(events.input);

        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let mode_overlay = load(P_MODE) >= MODE_OVERLAY_THRESHOLD;
        let bypassed = self.shared.bypass.load(Ordering::Relaxed);

        let Some(mut main_pair) = audio.port_pair(0) else {
            return Ok(ProcessStatus::Continue);
        };
        let Some(channel_pairs) = main_pair.channels()?.into_f32() else {
            return Ok(ProcessStatus::Continue);
        };
        let mut iter = channel_pairs.into_iter();
        let Some(ch_l) = iter.next() else {
            return Ok(ProcessStatus::Continue);
        };
        let ch_r = iter.next();

        let (read_l, write_l) = clap_helpers::split_io_parts(ch_l);
        let (read_r, write_r) = match ch_r {
            Some(c) => clap_helpers::split_io_parts(c),
            None => (None, None),
        };
        let Some(write_l) = write_l else {
            return Ok(ProcessStatus::Continue);
        };
        let frames = write_l.len().min(self.in_l.len());

        // Snapshot input into pre-allocated scratch — Overlay reads it,
        // Instrument ignores it. Missing/short input reads as silence
        // (lesson 15: OutputOnly buffers are real and common for synths).
        match read_l {
            Some(r) => {
                let k = frames.min(r.len());
                self.in_l[..k].copy_from_slice(&r[..k]);
                self.in_l[k..frames].fill(0.0);
            }
            None => self.in_l[..frames].fill(0.0),
        }
        match read_r.or(read_l) {
            Some(r) => {
                let k = frames.min(r.len());
                self.in_r[..k].copy_from_slice(&r[..k]);
                self.in_r[k..frames].fill(0.0);
            }
            None => self.in_r[..frames].fill(0.0),
        }

        if bypassed {
            if mode_overlay {
                write_l[..frames].copy_from_slice(&self.in_l[..frames]);
            } else {
                write_l[..frames].fill(0.0);
            }
            if let Some(wr) = write_r {
                if mode_overlay {
                    wr[..frames].copy_from_slice(&self.in_r[..frames]);
                } else {
                    wr[..frames].fill(0.0);
                }
            }
            return Ok(ProcessStatus::Continue);
        }

        let sr = self.sample_rate;
        let breath = load(P_BREATH).clamp(0.0, 1.0);
        let jitter = load(P_JITTER).clamp(0.0, 1.0);
        let shimmer = load(P_SHIMMER).clamp(0.0, 1.0);
        let tone = load(P_TONE).clamp(0.0, 1.0);
        let formant_st = load(P_FORMANT);
        let attack_s = (load(P_ATTACK) / 1000.0).max(0.0005);
        let release_s = (load(P_RELEASE) / 1000.0).max(0.0005);
        let chiff = load(P_CHIFF).clamp(0.0, 1.0);
        let color = load(P_COLOR).clamp(0.0, 1.0);
        let howl = load(P_HOWL).clamp(0.0, 1.0);
        let mix = load(P_MIX).clamp(0.0, 1.0);
        let output_db = load(P_OUTPUT);
        let gust_amt = load(P_GUST).clamp(0.0, 1.0);
        let whistle = load(P_WHISTLE).clamp(0.0, 1.0);

        // ONE shared gust-surge envelope — a real gust doesn't hit every
        // polyphonic voice independently, and Overlay's wind-bed amplitude +
        // input-ducking filter must move together (see `render_overlay`). Rate
        // maps 0.05-0.5 Hz.
        //
        // Advanced once per SAMPLE into a per-block curve. It used to be called
        // once per block, but `WobbleGen` takes exactly one one-pole step per
        // call with a coefficient derived from `sr` — so at 256-frame blocks it
        // ran 256x slower than its own rate control claimed, and changing the
        // host's buffer size changed the gust period.
        let gust_rate_hz = 0.05 + 0.45 * gust_amt;
        for i in 0..frames {
            let wobble = self.gust_gen.next(sr, gust_rate_hz);
            let swell = (0.5 + 0.5 * wobble).clamp(0.0, 1.0);
            self.swell_curve[i] = swell;
            self.gust_curve[i] = (1.0 - gust_amt) + gust_amt * swell;
        }

        let shift = 2f32.powf(formant_st / 12.0);
        let formant_f = [
            BASE_FORMANT_F[0] * shift,
            BASE_FORMANT_F[1] * shift,
            BASE_FORMANT_F[2] * shift,
        ];

        if mode_overlay {
            render_overlay(
                &self.in_l[..frames],
                &self.in_r[..frames],
                &mut self.out_l_scratch[..frames],
                &mut self.out_r_scratch[..frames],
                &self.shared.scope,
                &mut self.overlay_howl,
                &mut self.overlay_env,
                &mut self.overlay_pitch,
                &mut self.duck_l,
                &mut self.duck_r,
                sr,
                breath,
                color,
                mix,
                howl,
                whistle,
                gust_amt,
                &self.swell_curve[..frames],
                &self.gust_curve[..frames],
                output_db,
            );
        } else {
            let harmonics = harmonics_from_tone(tone);
            let bend_st = self.shared.pitch_bend_st.load(Ordering::Relaxed);
            render_instrument(
                &mut self.voices,
                &mut self.out_l_scratch[..frames],
                &mut self.out_r_scratch[..frames],
                &self.shared.scope,
                sr,
                bend_st,
                harmonics,
                formant_f,
                breath,
                jitter,
                shimmer,
                chiff,
                color,
                howl,
                &self.gust_curve[..frames],
                whistle,
                attack_s,
                release_s,
                output_db,
            );
        }

        write_l[..frames].copy_from_slice(&self.out_l_scratch[..frames]);
        if let Some(wr) = write_r {
            wr[..frames].copy_from_slice(&self.out_r_scratch[..frames]);
        }

        Ok(ProcessStatus::Continue)
    }
}

// ===========================================================================
// CLAP audio ports — ONE stereo in-place main port (both directions).
// ===========================================================================

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 {
        1
    }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index != 0 {
            return;
        }
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: if is_input { b"Input" } else { b"Output" },
            channel_count: 2,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            in_place_pair: Some(ClapId::new(0)),
        });
    }
}

impl PluginNotePortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input {
            1
        } else {
            0
        }
    }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if !is_input || index != 0 {
            return;
        }
        writer.set(&NotePortInfo {
            id: ClapId::new(0),
            name: b"Notes",
            // Both dialects — a host that doesn't speak native CLAP notes
            // falls back to MIDI 1.0 (lesson 14).
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
        self.shared
            .params
            .get(i)
            .map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(
        &mut self,
        id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        use core::fmt::Write;
        let pid = id.get() as usize;
        if pid == P_MODE {
            return write!(writer, "{}", if value < 0.5 { "Instrument" } else { "Overlay" });
        }
        if pid == P_PRESET {
            if let Some(r) = superduper_dsp_sdk::clap_helpers::preset_value_to_text(
                |i| PRESETS.get(i).map(|p| p.name),
                value,
                writer,
            ) {
                return r;
            }
        }
        ParamDef::write_display(PARAMS, id, value, writer)
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        if id.get() as usize == P_PRESET {
            if let Some(v) = superduper_dsp_sdk::clap_helpers::preset_text_to_value(
                PRESET_COUNT,
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
        if let Some(idx) = superduper_dsp_sdk::clap_helpers::preset_recall_target(
            self.shared.params[P_PRESET].load(Ordering::Relaxed),
            &self.shared.active_preset,
            PRESETS.len(),
        ) {
            apply_preset_idx(&self.shared.inner, idx);
        }
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
    }
}

// ===========================================================================
// CLAP state — params + bypass + active preset, via the shared SDK helper.
// Wind carries no custom non-param data (no drawn curves, no harmonic
// table), so the simple macro covers the whole state.
// ===========================================================================

superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);

// ===========================================================================
// CLAP GUI extension.
// ===========================================================================

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
        Some(GuiConfiguration {
            api_type,
            is_floating: false,
        })
    }
    fn create(&mut self, _: GuiConfiguration) -> Result<(), PluginError> {
        Ok(())
    }
    fn destroy(&mut self) {
        self.gui_handle = None;
    }
    fn set_scale(&mut self, _: f64) -> Result<(), PluginError> {
        Ok(())
    }
    fn get_size(&mut self) -> Option<GuiSize> {
        Some(GuiSize {
            width: self.gui_resize.0.load(Ordering::Relaxed),
            height: self.gui_resize.1.load(Ordering::Relaxed),
        })
    }
    fn can_resize(&mut self) -> bool {
        true
    }
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
        let handle =
            gui::open_window(&window, self.shared.shared_handle(), self.gui_resize.clone());
        self.gui_handle = Some(handle);
        Ok(())
    }
    fn set_transient(&mut self, _: ClapGuiWindow) -> Result<(), PluginError> {
        Ok(())
    }
    fn show(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
    fn hide(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
}

// ===========================================================================
// Factory.
// ===========================================================================

pub struct SuperDuperWind;

impl Plugin for SuperDuperWind {
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

impl DefaultPluginFactory for SuperDuperWind {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("co.superduperai.wind", plugin_display_name!("SuperDuper Wind"))
            .with_vendor("SuperDuperAI")
            .with_version(version_string!("0.1"))
            .with_description(
                "Breath/wind instrument — additive tone + formant-shaped stochastic \
                 breath noise (SMS). Instrument (poly synth) or Overlay (adds breath \
                 on top of any audio) mode.",
            )
            // Has both an audio port AND a note port, same shape as the
            // vocoder — classify as an effect, not a pure instrument, per
            // CLAUDE.md's guidance to mirror the vocoder's category choice.
            .with_features([AUDIO_EFFECT, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Wind — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperWind>);

#[allow(dead_code)]
fn _meta() -> (&'static str, &'static str) {
    (build_num!(), build_date!())
}
