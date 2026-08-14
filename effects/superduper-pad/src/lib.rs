//! SuperDuper Pad — polyphonic MIDI-driven pad synthesizer.
//!
//! Each held note allocates a voice from an 8-strong polyphonic pool.
//! A voice = stereo pair of `PadVoice` (four-partial wavetable-flavoured
//! oscillator with built-in Chamberlin SVF + tanh) wrapped in an ADSR
//! envelope. Re-triggering the same key on top of a still-releasing tail
//! re-uses the same voice so re-attacks don't glitch.
//!
//! Voice stealing: idle → quietest releasing → oldest. Sample-accurate
//! note timing via `events.input.batch()`.
//!
//! CLAP feature flags: INSTRUMENT, STEREO, SYNTHESIZER. Note input port
//! advertises both CLAP and MIDI 1.0 dialects so DAWs can pick whichever
//! they prefer.

#![allow(clippy::missing_safety_doc)]

pub mod gui;
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
use clack_common::stream::{InputStream, OutputStream};
use clack_extensions::state::{PluginState, PluginStateImpl};

use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{
    AdsrEnvelope, AdsrParams, PadParams, PadVoice, SmoothedParam, midi_note_to_hz,
};

use presets::PRESETS;

// ---------------------------------------------------------------------------
// Logging — file in ~/.superduper-dsp/pad.log. Same pattern as Ambient.
// ---------------------------------------------------------------------------

fn init_logging() { superduper_dsp_sdk::log::init("pad"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Params — 10 controls: filter (2) + motion (3) + ADSR (4) + output (1).
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Cutoff",     min: 80.0, max: 16000.0, default: 3500.0, unit: "Hz"    },
    ParamDef { id: 1, name: b"Resonance",  min: 0.0,  max: 0.9,     default: 0.18,   unit: ""      },
    ParamDef { id: 2, name: b"Modulation", min: 0.0,  max: 50.0,    default: 6.0,    unit: "cents" },
    ParamDef { id: 3, name: b"Drive",      min: 0.0,  max: 1.0,     default: 0.2,    unit: ""      },
    ParamDef { id: 4, name: b"Width",      min: 0.0,  max: 30.0,    default: 8.0,    unit: "cents" },
    ParamDef { id: 5, name: b"Attack",     min: 0.001, max: 4.0,    default: 0.4,    unit: "s"     },
    ParamDef { id: 6, name: b"Decay",      min: 0.01, max: 4.0,     default: 0.6,    unit: "s"     },
    ParamDef { id: 7, name: b"Sustain",    min: 0.0,  max: 1.0,     default: 0.8,    unit: ""      },
    ParamDef { id: 8, name: b"Release",    min: 0.01, max: 8.0,     default: 1.5,    unit: "s"     },
    ParamDef { id: 9, name: b"Output",     min: -36.0, max: 6.0,    default: -8.0,   unit: "dB"    },
    ParamDef { id: 10, name: b"Bend Range", min: 0.0,  max: 24.0,    default: 2.0,    unit: "ST"    },
    // DAHDSR extensions — pre-attack silence ("Delay") + peak hold
    // before decay. Both default to 0 → classic ADSR behaviour.
    ParamDef { id: 11, name: b"Env Delay",  min: 0.0,  max: 4.0,     default: 0.0,    unit: "s"     },
    ParamDef { id: 12, name: b"Env Hold",   min: 0.0,  max: 4.0,     default: 0.0,    unit: "s"     },
    // Polyphony cap — clamped to [2, 16]. New voice allocations beyond
    // this count steal from the oldest releasing voice.
    ParamDef { id: 13, name: b"Polyphony",  min: 2.0,  max: 16.0,    default: 8.0,    unit: "v"     },
    // Preset selector — stepped 0..N-1. Set from the host or an agent
    // (producer-pal / MCP) to recall a whole preset (timbre) without
    // opening the GUI. The recall (apply_preset) writes the preset's
    // param defaults + marks them dirty, which is allocation-free here
    // but mirrors Wave's main-thread recall pattern — see
    // PluginMainThreadParams::flush + on_main_thread.
    ParamDef { id: 14, name: b"Preset",     min: 0.0,  max: (presets::PRESET_COUNT - 1) as f64, default: 0.0, unit: "" },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[14];

pub const P_CUTOFF: usize = 0;
pub const P_RESONANCE: usize = 1;
pub const P_MODULATION: usize = 2;
pub const P_DRIVE: usize = 3;
pub const P_WIDTH: usize = 4;
pub const P_ATTACK: usize = 5;
pub const P_DECAY: usize = 6;
pub const P_SUSTAIN: usize = 7;
pub const P_RELEASE: usize = 8;
pub const P_OUTPUT: usize = 9;
pub const P_BEND_RANGE: usize = 10;
pub const P_ENV_DELAY: usize = 11;
pub const P_ENV_HOLD: usize = 12;
pub const P_POLYPHONY: usize = 13;
pub const P_PRESET: usize = 14;

/// Hard cap on voice array size. The Polyphony param limits how many
/// of these the user can actually trigger; raising the array gives
/// headroom for the cap without re-allocating at runtime.
pub const VOICE_COUNT: usize = 16;

// ---------------------------------------------------------------------------
// Shared params — identical pattern to Ambient.
// ---------------------------------------------------------------------------

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: std::sync::atomic::AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub midi_learn: superduper_synth_core::gui::MidiLearnState,
    pub dirty_params: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_begin: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_end: [std::sync::atomic::AtomicBool; PARAMS.len()],
    /// Currently-selected preset index — persisted via simple_state
    /// so the dropdown survives project reopens.
    pub active_preset: std::sync::atomic::AtomicU32,
    /// Live polyphony count for the GUI / metering. Updated each block from
    /// the audio thread (Relaxed store).
    pub active_voices: std::sync::atomic::AtomicU32,
    /// Live pitch-bend in semitones (signed). Set by MIDI 0xE0, read by
    /// the audio thread when computing voice frequency.
    pub pitch_bend_st: AtomicF32,
}

pub struct PluginShared {
    pub inner: SharedParams,
}

impl PluginShared {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: std::sync::atomic::AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                active_preset: std::sync::atomic::AtomicU32::new(0),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
                midi_learn: superduper_synth_core::gui::MidiLearnState::new(),
                active_voices: std::sync::atomic::AtomicU32::new(0),
                pitch_bend_st: AtomicF32::new(0.0),
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

/// Recall a factory preset by index — used by both the GUI preset picker
/// and the host-controllable Preset param (producer-pal / MCP). Writes the
/// preset's full param value vector into the shared atomics (SKIPPING the
/// Preset selector itself), marks every changed param dirty so the host's
/// LOM / automation lane — and an agent reading params — observes the recall
/// (lesson 21d), then records the active index. Allocation is fine because
/// callers run this on the MAIN thread only (GUI, on_main_thread, main-thread
/// flush) — NEVER from process()/audio flush.
pub fn apply_preset(shared: &SharedParamsInner, idx: usize) {
    let Some(preset) = PRESETS.get(idx) else { return };
    for (i, &v) in preset.values.iter().enumerate() {
        if i == P_PRESET {
            continue; // never clobber the selector itself
        }
        if let Some(atom) = shared.params.get(i) {
            atom.store(v, Ordering::Relaxed);
            shared.dirty_params[i].store(true, Ordering::Relaxed);
        }
    }
    // Reflect the recalled preset back into the selector param so the host
    // and producer-pal/MCP read the active index, and record the switch.
    if let Some(atom) = shared.params.get(P_PRESET) {
        atom.store(idx as f32, Ordering::Relaxed);
    }
    shared.dirty_params[P_PRESET].store(true, Ordering::Relaxed);
    shared
        .active_preset
        .store(idx as u32, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Voice — one PadVoice per stereo side plus an ADSR envelope. Identifies
// itself by MIDI key + optional note_id so NoteOff finds the right voice
// even when the user retriggers between channels.
// ---------------------------------------------------------------------------

const NOTE_FREE: u8 = 0xff;

#[derive(Clone, Copy)]
struct Voice {
    voice_l: PadVoice,
    voice_r: PadVoice,
    env: AdsrEnvelope,
    /// MIDI key 0..127, or NOTE_FREE when slot is unallocated.
    key: u8,
    /// CLAP note_id (-1 = no specific id). Required so NoteOff targeting a
    /// note_id matches even after the same key has been re-pressed.
    note_id: i32,
    velocity: f32,
    /// Monotonic stamp — used by the voice stealer to find the oldest
    /// allocated voice when all slots are busy.
    age_stamp: u64,
    /// Choke-fade state. When `choke_remaining > 0` the voice ignores its
    /// ADSR and applies a linear amplitude ramp from `choke_level` to 0
    /// over `choke_total` samples, then frees the slot. CLAP NoteChoke
    /// and MIDI All-Sound-Off (CC 120) trigger this path — REAPER sends
    /// CC 120 on transport relocate, which used to hard-cut the envelope
    /// and click audibly. 5 ms fade is below the perceptual click
    /// threshold yet short enough to feel instantaneous.
    choke_remaining: u32,
    choke_total: u32,
    choke_level: f32,
    /// Deferred-steal parking. A busy voice that gets stolen is choke-faded to
    /// silence (old note keeps sounding during the fade) and the new note is
    /// parked here; the render loop starts it from silence when the fade ends,
    /// so a stolen full-amplitude voice never hard-swaps pitch and clicks.
    /// `pending_key == NOTE_FREE` means nothing parked.
    pending_key: u8,
    pending_note_id: i32,
    pending_velocity: f32,
    /// MPE per-voice pitch offset in semitones. CLAP NoteExpression
    /// (Tuning channel) writes per-note bend here so each voice can be
    /// pitched independently — Roli Seaboard / Linnstrument workflows.
    pitch_offset_st: f32,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            voice_l: PadVoice::default(),
            voice_r: PadVoice::default(),
            env: AdsrEnvelope::default(),
            key: NOTE_FREE,
            note_id: -1,
            velocity: 0.0,
            age_stamp: 0,
            choke_remaining: 0,
            choke_total: 0,
            choke_level: 0.0,
            pending_key: NOTE_FREE,
            pending_note_id: -1,
            pending_velocity: 0.0,
            pitch_offset_st: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Audio processor — owns the voice pool + smoothed params.
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {
    /// Host main-thread callback — the audio thread calls request_callback()
    /// when the Preset param moved off the last-applied index; here (main
    /// thread) the (allocation-allowed) recall is legal.
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
    /// Preset param change needs the main-thread apply_preset recall.
    host: HostAudioProcessorHandle<'a>,
    voices: [Voice; VOICE_COUNT],
    /// Wraps; only used relatively when comparing voice ages.
    next_age: u64,
    smooth_cutoff: SmoothedParam,
    smooth_resonance: SmoothedParam,
    smooth_modulation: SmoothedParam,
    smooth_drive: SmoothedParam,
    smooth_width: SmoothedParam,
    smooth_output: SmoothedParam,
    sample_rate: f32,
}

/// Match against a `Match<u16>` field — `Match::All` matches any key, a
/// `Specific` matches only that exact key. CLAP uses this for wildcard
/// note targeting (e.g. AllNotesOff is NoteOff with key = Match::All).
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

        let polyphony = self.shared.params[P_POLYPHONY]
            .load(Ordering::Relaxed)
            .clamp(2.0, VOICE_COUNT as f32) as usize;

        // 1. Retrigger same key — single voice per held note avoids zombie
        //    voices when a host sends rapid on/off pairs. Cancels any
        //    in-flight choke fade so a held key over CC 120 still sounds.
        for v in self.voices.iter_mut() {
            if v.key == key && v.note_id == note_id {
                // Legato re-attack from the current level. `retrigger` (not
                // `gate_on`) avoids the PreDelay level=0 drop that would silence
                // a still-sounding held voice for a sample → a click when the
                // same key is replayed over a sustained pad.
                v.env.retrigger();
                v.velocity = velocity;
                v.age_stamp = stamp;
                v.choke_remaining = 0;
                v.pitch_offset_st = 0.0;
                return;
            }
        }
        // Count currently-active voices to enforce the polyphony cap.
        // If we're already at the cap we skip straight to voice-steal
        // logic instead of spinning up a fresh slot from the idle pool.
        let active_count = self.voices.iter().filter(|v| v.key != NOTE_FREE).count();
        let cap_hit = active_count >= polyphony;
        // 2. Free voice — assign without touching PadVoice/filter state.
        //    A full `Voice::default()` reset zeroes the SVF integrators
        //    and resets the oscillator phases, which causes an audible
        //    click on any new note. Re-using whatever state the voice
        //    had left (it was idle, so amplitude is 0 anyway) lets the
        //    attack ramp smoothly from silence with the same oscillator
        //    continuity.
        if !cap_hit {
            if let Some(v) = self.voices.iter_mut().find(|v| v.env.is_idle() && v.choke_remaining == 0) {
                v.key = key;
                v.note_id = note_id;
                v.velocity = velocity;
                v.age_stamp = stamp;
                v.env = AdsrEnvelope::default();
                v.env.gate_on();
                v.choke_remaining = 0;
                v.pitch_offset_st = 0.0;
                return;
            }
        }
        // 3. Quietest releasing voice — skip voices already in a deferred-steal
        //    fade (`choke_remaining > 0`): re-choking one resets its ramp (a
        //    click) and clobbers the note parked on it. It frees in ~4 ms.
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
        // 4. Oldest voice by stamp (smallest age_stamp = oldest).
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
        // Deferred steal — the old note is at full amplitude, so replacing it
        // in place (even with preserved oscillator state) steps the sound. Instead
        // choke-fade the old note to silence over ~4 ms and park the new note;
        // the render loop starts it from silence when the fade ends → 0→0, no click.
        let fade_samples = ((self.sample_rate * 0.004) as u32).max(1);
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
            if v.key == NOTE_FREE {
                continue;
            }
            if matches_key(key_match, v.key) && matches_note_id(note_id_match, v.note_id) {
                v.env.gate_off();
            }
        }
    }

    fn choke_voice(&mut self, key_match: Match<u16>, note_id_match: Match<u32>) {
        // CLAP NoteChoke / MIDI All-Sound-Off (CC 120) is "die now". A true
        // hard cut (env = 0 in one sample) produces an audible click equal
        // to the current voice amplitude — REAPER hits this on every
        // transport relocate. 5 ms linear fade is below the perceptual click
        // threshold (single-cycle period at 200 Hz) but short enough to feel
        // instantaneous to a human. The voice keeps rendering through the
        // ramp; render_subblock frees the slot when choke_remaining reaches 0.
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
        // MIDI 1.0 status nibble determines what we do. We only handle the
        // ones that translate to voice events; everything else (CC, pitch
        // bend, aftertouch, program change) is ignored for now.
        let status = data[0] & 0xf0;
        let key = data[1];
        let raw_velocity = data[2];
        match status {
            // Note on (with velocity 0 = note off — standard MIDI quirk).
            0x90 => {
                if raw_velocity == 0 {
                    self.release_voice(Match::Specific(key as u16), Match::All);
                } else {
                    let velocity = raw_velocity as f32 / 127.0;
                    self.allocate_voice(key, velocity, -1);
                }
            }
            0x80 => {
                self.release_voice(Match::Specific(key as u16), Match::All);
            }
            // Pitch bend (status 0xE0). 14-bit value, 8192 = center,
            // mapped to ±BendRange semitones.
            0xe0 => {
                let raw = (key as i32) | ((raw_velocity as i32) << 7);
                let centered = raw - 8192;
                let normalised = (centered as f32) / 8191.0;
                let range = self.shared.params[P_BEND_RANGE].load(Ordering::Relaxed);
                self.shared
                    .pitch_bend_st
                    .store(normalised * range, Ordering::Relaxed);
            }
            // All notes off (CC 123).
            0xb0 if key == 123 => {
                self.release_voice(Match::All, Match::All);
            }
            // All sound off (CC 120) — hard cut.
            0xb0 if key == 120 => {
                self.choke_voice(Match::All, Match::All);
            }
            // Expressive CC mapping. Stored directly (no dirty flag) so
            // the plugin doesn't echo CC into the FX automation lane —
            // CC moves stay in the MIDI clip.
            //   CC  1 ModWheel    → Modulation depth (cents)
            //   CC 11 Expression  → Drive
            //   CC 71 Resonance   → Resonance
            //   CC 74 Brightness  → Cutoff (log-mapped)
            0xb0 => {
                let v = raw_velocity as f32 / 127.0;
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
                        1 => lin(P_MODULATION, v),
                        11 => lin(P_DRIVE, v),
                        71 => lin(P_RESONANCE, v),
                        74 => log_cutoff(v),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_note_event(&mut self, ev: &CoreEventSpace<'_>) {
        match ev {
            CoreEventSpace::NoteOn(n) => {
                let key = match n.key() {
                    Match::Specific(k) => k as u8,
                    Match::All => return, // wildcard note-on is meaningless
                };
                let velocity = n.velocity().clamp(0.0, 1.0) as f32;
                let note_id = match n.note_id() {
                    Match::Specific(id) => id as i32,
                    Match::All => -1,
                };
                self.allocate_voice(key, velocity, note_id);
            }
            CoreEventSpace::NoteOff(n) => {
                self.release_voice(n.key(), n.note_id());
            }
            CoreEventSpace::NoteChoke(n) => {
                self.choke_voice(n.key(), n.note_id());
            }
            CoreEventSpace::Midi(m) => {
                self.handle_midi_event(m.data());
            }
            // CLAP NoteExpression with NoteExpressionType::Tuning carries
            // per-voice pitch bend in semitones (MPE pitch slide). Find
            // the matching voice by note_id (or key as fallback) and
            // update its pitch_offset_st atom.
            CoreEventSpace::NoteExpression(ne) => {
                use clack_common::events::event_types::NoteExpressionType;
                if ne.expression_type() == Some(NoteExpressionType::Tuning) {
                    let target_note_id = match ne.note_id() {
                        Match::Specific(id) => id as i32,
                        Match::All => -1,
                    };
                    let target_key = match ne.key() {
                        Match::Specific(k) => Some(k as u8),
                        Match::All => None,
                    };
                    let st = ne.value() as f32;
                    for v in self.voices.iter_mut() {
                        if v.key == NOTE_FREE { continue; }
                        let id_ok = target_note_id == -1 || v.note_id == target_note_id;
                        let key_ok = target_key.map_or(true, |k| v.key == k);
                        if id_ok && key_ok {
                            v.pitch_offset_st = st;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Render a sub-block of [start, end) into the stereo output buffers.
    fn render_subblock(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        cutoff_target: f32,
        resonance_target: f32,
        modulation_target: f32,
        drive_target: f32,
        width_target: f32,
        output_target: f32,
        attack_s: f32,
        decay_s: f32,
        sustain: f32,
        release_s: f32,
        delay_s: f32,
        hold_s: f32,
    ) {
        let sr = self.sample_rate;
        debug_assert_eq!(out_l.len(), out_r.len());

        for i in 0..out_l.len() {
            let cutoff = self.smooth_cutoff.step(cutoff_target, sr);
            let resonance = self.smooth_resonance.step(resonance_target, sr);
            let modulation = self.smooth_modulation.step(modulation_target, sr);
            let drive = self.smooth_drive.step(drive_target, sr);
            let width = self.smooth_width.step(width_target, sr);
            let output_db = self.smooth_output.step(output_target, sr);

            let adsr_p = AdsrParams {
                sr,
                delay_s,
                attack_s,
                hold_s,
                decay_s,
                sustain,
                release_s,
            };

            let mut mix_l = 0.0_f32;
            let mut mix_r = 0.0_f32;
            for v in self.voices.iter_mut() {
                if v.key == NOTE_FREE && v.env.is_idle() && v.choke_remaining == 0 {
                    continue;
                }
                // Choke fade path — overrides the ADSR with a linear ramp
                // from choke_level → 0 across choke_total samples. Voice
                // keeps generating audio (oscillator + filter continue
                // ticking) so the fade is a multiplicative window on a
                // bandlimited signal — no truncation discontinuity.
                if v.choke_remaining > 0 {
                    let fade = (v.choke_remaining as f32) / (v.choke_total as f32);
                    let base_hz = midi_note_to_hz(v.key as f32 + self.shared.pitch_bend_st.load(Ordering::Relaxed) + v.pitch_offset_st);
                    let l_hz = base_hz * 2f32.powf(-width * 0.5 / 1200.0);
                    let r_hz = base_hz * 2f32.powf(width * 0.5 / 1200.0);
                    let pl = PadParams {
                        sr,
                        root_hz: l_hz,
                        cutoff_hz: cutoff,
                        resonance,
                        modulation_cents: modulation,
                        drive,
                    };
                    let pr = PadParams { root_hz: r_hz, ..pl };
                    let amp = fade * v.choke_level * v.velocity;
                    mix_l += v.voice_l.process(pl) * amp;
                    mix_r += v.voice_r.process(pr) * amp;
                    v.choke_remaining -= 1;
                    if v.choke_remaining == 0 {
                        if v.pending_key != NOTE_FREE {
                            // Deferred steal fade done — start the parked note
                            // from silence (env attacks from 0 → no click). Keep
                            // the oscillator/filter state (lesson 17).
                            v.key = v.pending_key;
                            v.note_id = v.pending_note_id;
                            v.velocity = v.pending_velocity;
                            v.pending_key = NOTE_FREE;
                            v.pitch_offset_st = 0.0;
                            v.env = AdsrEnvelope::default();
                            v.env.gate_on();
                        } else {
                            v.env = AdsrEnvelope::default();
                            v.key = NOTE_FREE;
                        }
                    }
                    continue;
                }
                let env = v.env.process(adsr_p);
                if env <= 1e-5 && v.env.is_idle() {
                    // Free the slot — env hit silence after release.
                    v.key = NOTE_FREE;
                    continue;
                }
                let base_hz = midi_note_to_hz(v.key as f32 + self.shared.pitch_bend_st.load(Ordering::Relaxed) + v.pitch_offset_st);
                let l_hz = base_hz * 2f32.powf(-width * 0.5 / 1200.0);
                let r_hz = base_hz * 2f32.powf(width * 0.5 / 1200.0);

                let pl = PadParams {
                    sr,
                    root_hz: l_hz,
                    cutoff_hz: cutoff,
                    resonance,
                    modulation_cents: modulation,
                    drive,
                };
                let pr = PadParams { root_hz: r_hz, ..pl };
                let amp = env * v.velocity;
                mix_l += v.voice_l.process(pl) * amp;
                mix_r += v.voice_r.process(pr) * amp;
            }
            // 8 voices summed — clamp headroom by a fixed scaler. Velocities
            // already attenuate so this only fires under chord stacks.
            let voice_scale = 0.5_f32;
            let out_lin = 10f32.powf(output_db / 20.0);
            let final_l = mix_l * voice_scale * out_lin;
            let final_r = mix_r * voice_scale * out_lin;
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
        slog!("activate sr={}", sr);
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        Ok(Self {
            shared,
            host,
            voices: [Voice::default(); VOICE_COUNT],
            next_age: 0,
            smooth_cutoff: SmoothedParam::new(load(P_CUTOFF)),
            smooth_resonance: SmoothedParam::new(load(P_RESONANCE)),
            smooth_modulation: SmoothedParam::new(load(P_MODULATION)),
            smooth_drive: SmoothedParam::new(load(P_DRIVE)),
            smooth_width: SmoothedParam::new(load(P_WIDTH)),
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
        // Flush denormals to zero — long decays / feedback loops
        // otherwise generate ≈10⁻³⁸ floats that murder CPU and cause
        // periodic ticks at the buffer rate. RAII restores host CSR.
        let _denormals = superduper_dsp_sdk::denormals::Guard::new();
        // Flush GUI-driven param changes back to the host so REAPER can
        // record the move into the automation lane.
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(
            &self.shared.params, &self.shared.dirty_params, events.output);
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(
            &self.shared.gesture_begin,
            &self.shared.gesture_end,
            events.output,
        );
        // Preset selector: if the host or producer-pal/MCP moved the Preset
        // param, wake the main thread to recall it — apply_preset is a
        // main-thread operation (mirrors the Wave reference, where the recall
        // allocates mip tables). The audio thread only requests the callback.
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

        // Walk the output port. Pad is a generator — we ignore any input
        // side and only write to outputs.
        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else {
                continue;
            };
            let mut writers: Vec<_> = channel_pairs
                .into_iter()
                .filter_map(superduper_dsp_sdk::clap_helpers::output_slice)
                .collect();
            if writers.len() < 2 {
                // Mono output isn't useful for a stereo pad; silence and bail.
                for w in writers.iter_mut() {
                    w.fill(0.0);
                }
                continue;
            }

            // The clack `Vec` allocates once per process — this is the
            // standard pattern in the project's other effects. Acceptable
            // because it doesn't grow under steady state (one entry per
            // channel) and clack handles the lifetime correctly.
            //
            // To avoid this we'd need to take two &mut into the same Vec
            // which Rust forbids — keep the simple version and accept the
            // tiny per-block alloc cost.
            let (a, b) = writers.split_at_mut(1);
            let out_l: &mut [f32] = a[0];
            let out_r: &mut [f32] = b[0];
            let frames = out_l.len().min(out_r.len());

            if bypassed {
                out_l[..frames].fill(0.0);
                out_r[..frames].fill(0.0);
                continue;
            }

            // Sample-accurate event batching. Each batch only has events at
            // its first sample; the sub-block in between is event-free so we
            // can render it as a plain loop. Param targets are re-read inside
            // the loop since the batch's events may have updated them.
            for batch in events.input.batch() {
                // 1. apply note + param events at batch start
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

                // 2. render this sub-block
                let start = batch.first_sample().min(frames);
                let end = batch.next_batch_first_sample().unwrap_or(frames).min(frames);
                if end <= start {
                    continue;
                }
                // Reload param targets — events at this sample may have
                // updated the atomics.
                let cutoff = self.shared.params[P_CUTOFF].load(Ordering::Relaxed);
                let resonance = self.shared.params[P_RESONANCE].load(Ordering::Relaxed);
                let modulation = self.shared.params[P_MODULATION].load(Ordering::Relaxed);
                let drive = self.shared.params[P_DRIVE].load(Ordering::Relaxed);
                let width = self.shared.params[P_WIDTH].load(Ordering::Relaxed);
                let output = self.shared.params[P_OUTPUT].load(Ordering::Relaxed);
                let attack = self.shared.params[P_ATTACK].load(Ordering::Relaxed);
                let decay = self.shared.params[P_DECAY].load(Ordering::Relaxed);
                let sustain = self.shared.params[P_SUSTAIN].load(Ordering::Relaxed);
                let release = self.shared.params[P_RELEASE].load(Ordering::Relaxed);
                let env_delay = self.shared.params[P_ENV_DELAY].load(Ordering::Relaxed);
                let env_hold = self.shared.params[P_ENV_HOLD].load(Ordering::Relaxed);

                self.render_subblock(
                    &mut out_l[start..end],
                    &mut out_r[start..end],
                    cutoff,
                    resonance,
                    modulation,
                    drive,
                    width,
                    output,
                    attack,
                    decay,
                    sustain,
                    release,
                    env_delay,
                    env_hold,
                );
            }

            // Quiet any extra output channels (we only render L/R; if the
            // host gave us a surround port, downstream silence is safer than
            // garbage).
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
// CLAP extensions — audio ports, note ports, params, GUI.
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
            // Advertise both — DAW can pick MIDI 1.0 if it doesn't speak
            // native CLAP note events.
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
        v: f64,
        w: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        let pid = id.get() as usize;
        // Preset selector: show the preset name instead of "0.00".
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
        // Let "Choir" / "Glass" etc. resolve to the preset index too.
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
        // Preset selector recall — main thread, so apply_preset is legal here.
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
// CLAP state — params + bypass through the shared SDK helper. Without this
// REAPER drops everything when saving the project / FX chain preset.
// ---------------------------------------------------------------------------

superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);


use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window as ClapGuiWindow,
};
use std::sync::atomic::Ordering as AtomicOrdering;

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
        slog!("gui::create");
        Ok(())
    }
    fn destroy(&mut self) {
        slog!("gui::destroy");
        self.gui_handle = None;
    }
    fn set_scale(&mut self, _: f64) -> Result<(), PluginError> {
        Ok(())
    }
    fn get_size(&mut self) -> Option<GuiSize> {
        Some(GuiSize {
            width: self.gui_resize.0.load(AtomicOrdering::Relaxed),
            height: self.gui_resize.1.load(AtomicOrdering::Relaxed),
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
        self.gui_resize.0.store(w, AtomicOrdering::Relaxed);
        self.gui_resize.1.store(h, AtomicOrdering::Relaxed);
        Ok(())
    }
    fn set_parent(&mut self, window: ClapGuiWindow) -> Result<(), PluginError> {
        slog!("gui::set_parent");
        let handle = gui::open_window(&window, self.shared.shared_handle(), self.gui_resize.clone());
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

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct SuperDuperPad;

impl Plugin for SuperDuperPad {
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

impl DefaultPluginFactory for SuperDuperPad {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.pad",
            plugin_display_name!("SuperDuper Pad"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Polyphonic MIDI pad synth — four-partial PadVoice with ADSR")
        .with_features([INSTRUMENT, STEREO, SYNTHESIZER])
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!(
            "new_shared: Pad — build {} ({})",
            build_num!(),
            build_date!()
        );
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

clack_export_entry!(SinglePluginEntry<SuperDuperPad>);
