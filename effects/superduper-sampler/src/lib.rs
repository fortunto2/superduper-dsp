//! SuperDuper Sampler — polyphonic WAV player. Scans known sample
//! folders on activate (~/Music/SuperDuper Samples/ +
//! ~/Music/Favorite 808s/) and plays the active WAV polyphonically
//! with pitch, ADSR and optional loop.
//!
//! Use case: load any one-shot — 808 kicks, vocal phrases,
//! breakbeats, percussion — and play across the keyboard.
//! Out-of-the-box bass synth too: pitch a low note + long Decay
//! and you get a hard-hit 808-style sub.

#![allow(clippy::missing_safety_doc)]

pub mod bank;
pub mod gui;
pub mod voice;

use atomic_float::AtomicF32;
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
use clack_common::stream::{InputStream, OutputStream};
use clack_extensions::state::{PluginState, PluginStateImpl};

use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use parking_lot::Mutex;
use superduper_dsp_sdk::clap_helpers::{output_slice, ParamDef};
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{AdsrParams, SvfMode};

/// Map the P_FILTER_TYPE param value to the SVF mode the voice uses.
/// 0 = Off (filter bypassed), 1 = LP, 2 = HP, 3 = BP, 4 = Notch.
#[inline]
pub fn filter_mode_from_param(v: f32) -> Option<SvfMode> {
    match v.round() as i32 {
        1 => Some(SvfMode::Lp),
        2 => Some(SvfMode::Hp),
        3 => Some(SvfMode::Bp),
        4 => Some(SvfMode::Notch),
        _ => None,
    }
}

/// Map the P_CUTOFF "MIDI-style" param (0..127) to Hz, log-spaced
/// from 20 Hz to 20 kHz so the slider feels musical (one octave
/// per ~18 units). Mirror in the GUI's value_to_text formatter so
/// the readout shows real Hz instead of the raw param value.
#[inline]
pub fn cutoff_units_to_hz(v: f32) -> f32 {
    let v = v.clamp(0.0, 127.0) / 127.0;
    20.0 * 1000f32.powf(v)
}

use bank::{empty_sample, load_sample, scan_folders, SampleData};
use voice::{SampleVoice, VoiceParams, NOTE_FREE};

const VOICE_COUNT: usize = 8;

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn init_logging() { superduper_dsp_sdk::log::init("sampler"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Param table
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    // Sample index — clamped at runtime to the actual scan results.
    // We expose 256 as the max so the host can record sample changes
    // without us re-broadcasting the param table on every scan.
    ParamDef { id: 0,  name: b"Sample",  min: 0.0,   max: 255.0,  default: 0.0,  unit: ""   },
    // Pitch root — MIDI key at which the sample plays at its original speed.
    ParamDef { id: 1,  name: b"Root",    min: 0.0,   max: 127.0,  default: 60.0, unit: ""   },
    ParamDef { id: 2,  name: b"Tune",    min: -24.0, max: 24.0,   default: 0.0,  unit: "ST" },
    ParamDef { id: 3,  name: b"Fine",    min: -100.0, max: 100.0, default: 0.0,  unit: "ct" },
    // Loop
    ParamDef { id: 4,  name: b"Loop",       min: 0.0, max: 1.0, default: 0.0, unit: "" },
    ParamDef { id: 5,  name: b"Loop Start", min: 0.0, max: 1.0, default: 0.0, unit: "" },
    ParamDef { id: 6,  name: b"Loop End",   min: 0.0, max: 1.0, default: 1.0, unit: "" },
    // ADSR
    ParamDef { id: 7,  name: b"Attack",  min: 0.0,   max: 4.0,  default: 0.001, unit: "s" },
    ParamDef { id: 8,  name: b"Decay",   min: 0.01,  max: 8.0,  default: 0.5,   unit: "s" },
    ParamDef { id: 9,  name: b"Sustain", min: 0.0,   max: 1.0,  default: 1.0,   unit: ""  },
    ParamDef { id: 10, name: b"Release", min: 0.01,  max: 8.0,  default: 0.4,   unit: "s" },
    // Output
    ParamDef { id: 11, name: b"Output",  min: -36.0, max: 6.0,  default: -3.0,  unit: "dB" },
    // Playback trim — the slice of the sample that actually plays.
    // Both expressed as fractions of total sample length.
    ParamDef { id: 12, name: b"Start",   min: 0.0, max: 1.0, default: 0.0, unit: "" },
    ParamDef { id: 13, name: b"End",     min: 0.0, max: 1.0, default: 1.0, unit: "" },
    // Reverse — flip playback direction. Reads the slice backwards
    // from Trim End to Trim Start. Loop is disabled while reverse.
    ParamDef { id: 14, name: b"Reverse", min: 0.0, max: 1.0, default: 0.0, unit: "" },
    // Multi-mode filter. Type: 0 = Off, 1 = LP, 2 = HP, 3 = BP, 4 = Notch.
    ParamDef { id: 15, name: b"Filter",      min: 0.0,   max: 4.0,    default: 0.0,     unit: "" },
    // Cutoff stored in MIDI-like log units so the slider feels musical;
    // 0 = 20 Hz, 127 = 20 kHz, mapping `20 * 1000^(v/127)` (≈ 7 octaves).
    ParamDef { id: 16, name: b"Cutoff",      min: 0.0,   max: 127.0,  default: 95.0,    unit: "" },
    ParamDef { id: 17, name: b"Reso",        min: 0.0,   max: 0.97,   default: 0.1,     unit: "" },
    // Env→Cutoff in semitones. Positive = brighter on attack, negative
    // = darker. Driven by the amplitude ADSR — keeps the param table
    // compact instead of adding a dedicated filter envelope.
    ParamDef { id: 18, name: b"Env>Cutoff",  min: -60.0, max: 60.0,   default: 0.0,     unit: "ST" },
    // Velocity → amp. 0 = ignore velocity entirely (always full level),
    // 1 = velocity scales amp linearly (classic behaviour).
    ParamDef { id: 19, name: b"Vel>Amp",     min: 0.0,   max: 1.0,    default: 1.0,     unit: "" },
    // Velocity → cutoff in semitones. velocity * P_VEL_CUTOFF gets
    // added to the cutoff before the envelope. Positive = harder hit
    // is brighter; negative = harder hit is darker (rare but musical).
    ParamDef { id: 20, name: b"Vel>Cut",     min: -60.0, max: 60.0,   default: 0.0,     unit: "ST" },
];

pub const P_SAMPLE: usize = 0;
pub const P_ROOT: usize = 1;
pub const P_TUNE: usize = 2;
pub const P_FINE: usize = 3;
pub const P_LOOP: usize = 4;
pub const P_LOOP_START: usize = 5;
pub const P_LOOP_END: usize = 6;
pub const P_ATTACK: usize = 7;
pub const P_DECAY: usize = 8;
pub const P_SUSTAIN: usize = 9;
pub const P_RELEASE: usize = 10;
pub const P_OUTPUT: usize = 11;
pub const P_TRIM_START: usize = 12;
pub const P_TRIM_END: usize = 13;
pub const P_REVERSE: usize = 14;
pub const P_FILTER_TYPE: usize = 15;
pub const P_CUTOFF: usize = 16;
pub const P_RESO: usize = 17;
pub const P_ENV_CUTOFF: usize = 18;
pub const P_VEL_AMP: usize = 19;
pub const P_VEL_CUTOFF: usize = 20;

// ---------------------------------------------------------------------------
// Shared params + sample library
// ---------------------------------------------------------------------------

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub dirty_params: [AtomicBool; PARAMS.len()],
    pub gesture_begin: [AtomicBool; PARAMS.len()],
    pub gesture_end: [AtomicBool; PARAMS.len()],
    /// Currently-selected preset index — persisted via simple_state.
    pub active_preset: std::sync::atomic::AtomicU32,
    /// Active sample currently loaded — atomically swapped by the
    /// GUI thread when the user picks a different file. Audio thread
    /// clones the Arc when triggering a new voice; existing voices
    /// keep playing the previous sample until they finish.
    pub active_sample: Mutex<Arc<SampleData>>,
    /// Snapshot of the discovered sample files, each tagged with its
    /// pack (first subfolder). The GUI picks an index from this list
    /// and triggers a load; the audio thread never touches it.
    pub library: Mutex<Vec<bank::PackedSample>>,
    /// User-editable list of root folders scanned for WAV samples.
    /// Persisted to `~/.superduper-dsp/sampler-config.json` so the
    /// next session picks them up. Audio thread doesn't read it.
    pub sample_roots: Mutex<Vec<PathBuf>>,
    /// Currently-loaded library index. -1 = no sample yet.
    pub current_index: std::sync::atomic::AtomicI32,
    /// Plugin sample rate, captured at activate() so the GUI can show
    /// it in the status line.
    pub host_sr: AtomicF32,
    /// GUI-driven audition trigger. Stores a MIDI key (0..127) when
    /// the user clicks the waveform or Play button; the audio thread
    /// reads + clears it at the top of the next process() block and
    /// fires a NoteOn at that key. -1 = no pending trigger.
    pub audition_request: std::sync::atomic::AtomicI32,
}

pub struct PluginShared { pub inner: SharedParams }

impl PluginShared {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| AtomicBool::new(false)),
                active_preset: std::sync::atomic::AtomicU32::new(0),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
                active_sample: Mutex::new(empty_sample()),
                library: Mutex::new(Vec::new()),
                sample_roots: Mutex::new(bank::load_folders_config()),
                current_index: std::sync::atomic::AtomicI32::new(-1),
                host_sr: AtomicF32::new(48000.0),
                audition_request: std::sync::atomic::AtomicI32::new(-1),
            }),
        }
    }
    pub fn shared_handle(&self) -> SharedParams { Arc::clone(&self.inner) }
}

impl Default for PluginShared { fn default() -> Self { Self::new() } }
impl std::ops::Deref for PluginShared {
    type Target = SharedParamsInner;
    fn deref(&self) -> &SharedParamsInner { &self.inner }
}
impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

/// GUI helper: refresh the library scan, pick the i-th sample, decode
/// it and swap the active_sample Arc. Returns Ok(name) or Err(reason).
/// Called by the GUI when the user clicks a dropdown entry.
pub fn pick_sample(shared: &SharedParamsInner, idx: usize) -> Result<String, String> {
    let lib = shared.library.lock();
    let entry = lib.get(idx)
        .cloned()
        .ok_or_else(|| format!("sample index {} out of range ({})", idx, lib.len()))?;
    drop(lib);
    let data = load_sample(&entry.path).map_err(|e| e.to_string())?;
    let label = format!("{} / {}", entry.pack, data.display_name);
    *shared.active_sample.lock() = Arc::new(data);
    shared.current_index.store(idx as i32, Ordering::Relaxed);
    shared.params[P_SAMPLE].store(idx as f32, Ordering::Relaxed);
    shared.dirty_params[P_SAMPLE].store(true, Ordering::Relaxed);
    Ok(label)
}

/// Main-thread: if the `Sample` param points at a different library index
/// than the one currently loaded, decode that file and swap it in. This is
/// what makes the sampler **headless-selectable** — host automation, MCP, or
/// producer-pal can move the Sample param and get the audio to follow with no
/// GUI. Woken by the audio thread's `request_callback()` (→ `on_main_thread`)
/// and also driven by the main-thread param flush, so it works whether or not
/// the transport is running. Decoding allocates + does file I/O → main-thread
/// only, never call from `process()`. The param index is clamped to the
/// scanned library so an out-of-range automation value loads the last sample
/// instead of silently doing nothing.
pub fn maybe_load_pending_sample(shared: &SharedParamsInner) {
    let want = shared.params[P_SAMPLE].load(Ordering::Relaxed).round() as i32;
    if want < 0 {
        return;
    }
    let lib_len = shared.library.lock().len();
    if lib_len == 0 {
        return;
    }
    let idx = (want as usize).min(lib_len - 1);
    if idx as i32 == shared.current_index.load(Ordering::Relaxed) {
        return;
    }
    match pick_sample(shared, idx) {
        Ok(name) => slog!("headless sample load: idx {} -> {}", idx, name),
        Err(e) => slog!("headless sample load idx {} failed: {}", idx, e),
    }
}

/// GUI helper: rerun the folder scan using the user's edited folder
/// list and refresh the library. Returns the new entry count.
pub fn refresh_library(shared: &SharedParamsInner) -> usize {
    let folders = shared.sample_roots.lock().clone();
    let entries = scan_folders(&folders);
    let count = entries.len();
    *shared.library.lock() = entries;
    count
}

/// GUI helper: add a new sample root, persist the config, and rescan.
/// Skips paths that are empty, missing, or already in the list.
pub fn add_sample_root(shared: &SharedParamsInner, raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() { return Err("empty path".into()); }
    // Expand `~` to the home directory — by-far the most common form
    // a user types.
    let expanded = if let Some(rest) = trimmed.strip_prefix("~/") {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "HOME not set".to_string())?;
        home.join(rest)
    } else {
        PathBuf::from(trimmed)
    };
    if !expanded.exists() {
        return Err(format!("path doesn't exist: {}", expanded.display()));
    }
    {
        let mut roots = shared.sample_roots.lock();
        if roots.iter().any(|p| p == &expanded) {
            return Err("already in the list".into());
        }
        roots.push(expanded.clone());
        let _ = bank::save_folders_config(&roots);
    }
    refresh_library(shared);
    Ok(expanded.display().to_string())
}

/// Remove an entry by index, persist, rescan.
pub fn remove_sample_root(shared: &SharedParamsInner, idx: usize) {
    {
        let mut roots = shared.sample_roots.lock();
        if idx < roots.len() {
            roots.remove(idx);
            let _ = bank::save_folders_config(&roots);
        }
    }
    refresh_library(shared);
}

/// Reset folder list to the built-in defaults, persist, rescan.
pub fn reset_sample_roots(shared: &SharedParamsInner) {
    {
        let defaults = bank::default_sample_folders();
        *shared.sample_roots.lock() = defaults.clone();
        let _ = bank::save_folders_config(&defaults);
    }
    refresh_library(shared);
}

// ---------------------------------------------------------------------------
// Main thread + audio processor
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {
    /// Host main-thread callback — the audio thread calls `request_callback()`
    /// when the Sample param moved; here (off the RT thread) we decode + swap.
    fn on_main_thread(&mut self) {
        maybe_load_pending_sample(&self.shared.inner);
    }
}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    /// Host handle — used to wake the main thread (request_callback) when
    /// the Sample param is moved by host automation / MCP / producer-pal,
    /// so the (allocating, file-I/O) decode runs off the audio thread.
    host: HostAudioProcessorHandle<'a>,
    voices: [SampleVoice; VOICE_COUNT],
    next_age: u64,
    sample_rate: f32,
    /// Audition state. When the user clicks Play / the waveform, we
    /// trigger a NoteOn at the Root key and remember it here so we
    /// can deliver a matching NoteOff `audition_release_in` blocks
    /// later. Without that the voice's Sustain holds forever (at the
    /// default Sustain = 1.0) and the user has no way to stop it.
    audition_key: u8,
    audition_release_in: u32,
}

impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, PluginShared, PluginMainThread<'a>>
    for PluginAudioProcessor<'a>
{
    fn activate(
        host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut PluginMainThread<'a>,
        shared: &'a PluginShared,
        cfg: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        init_logging();
        let sr = cfg.sample_rate as f32;
        shared.host_sr.store(sr, Ordering::Relaxed);
        slog!("sampler activate sr={}", sr);
        Ok(Self {
            shared,
            host,
            voices: std::array::from_fn(|_| SampleVoice::default()),
            next_age: 0,
            sample_rate: sr,
            audition_key: 0,
            audition_release_in: 0,
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
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, events.input);
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(
            &self.shared.params, &self.shared.dirty_params, events.output);
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(
            &self.shared.gesture_begin, &self.shared.gesture_end, events.output);

        // If the Sample param was moved by the host / MCP / producer-pal to a
        // different library index than what's loaded, wake the main thread to
        // decode + swap it in. load_sample allocates + reads the file, so it's
        // forbidden here — we only request the callback (cheap, lock-free).
        let want = self.shared.params[P_SAMPLE].load(Ordering::Relaxed).round() as i32;
        if want >= 0 && want != self.shared.current_index.load(Ordering::Relaxed) {
            self.host.shared().request_callback();
        }

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        let sr = self.sample_rate;

        // GUI audition — fire one NoteOn for whichever key the GUI
        // requested (waveform click or Play button), then schedule a
        // matching NoteOff a few blocks later so the voice doesn't
        // hold forever on Sustain = 1.0.
        let req = self.shared.audition_request.swap(-1, Ordering::AcqRel);
        if req >= 0 && req < 128 {
            self.trigger(req as u8, 0.85, -1);
            self.audition_key = req as u8;
            // ~80 ms of attack/decay before we hand off to Release,
            // long enough for the user to perceive the transient body
            // of a percussion sample and for sustained samples to
            // start fading cleanly via the ADSR release tail.
            self.audition_release_in = 4;
        }
        if self.audition_release_in > 0 {
            self.audition_release_in -= 1;
            if self.audition_release_in == 0 {
                self.release(self.audition_key);
            }
        }

        // Walk events — NoteOn / NoteOff drive voice triggers.
        for batch in events.input.batch() {
            for ev in batch.events() {
                if let Some(core) = ev.as_core_event() {
                    match core {
                        CoreEventSpace::NoteOn(n) => {
                            let key = match n.key() {
                                Match::Specific(k) => k as u8,
                                _ => continue,
                            };
                            let velocity = n.velocity().clamp(0.0, 1.0) as f32;
                            let note_id = match n.note_id() {
                                Match::Specific(id) => id as i32,
                                _ => -1,
                            };
                            self.trigger(key, velocity, note_id);
                        }
                        CoreEventSpace::NoteOff(n) => {
                            let key = match n.key() {
                                Match::Specific(k) => k as u8,
                                _ => continue,
                            };
                            self.release(key);
                        }
                        CoreEventSpace::Midi(m) => {
                            let d = m.data();
                            let st = d[0] & 0xF0;
                            match st {
                                0x90 if d[2] > 0 => self.trigger(d[1], d[2] as f32 / 127.0, -1),
                                0x90 | 0x80 => self.release(d[1]),
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Audio render.
        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let params = VoiceParams {
            host_sr: sr,
            root_key: load(P_ROOT),
            tune_st: load(P_TUNE),
            fine_cents: load(P_FINE),
            loop_on: load(P_LOOP) >= 0.5,
            loop_start_frac: load(P_LOOP_START),
            loop_end_frac: load(P_LOOP_END),
            trim_start_frac: load(P_TRIM_START),
            trim_end_frac: load(P_TRIM_END),
            env: AdsrParams::adsr(sr, load(P_ATTACK), load(P_DECAY), load(P_SUSTAIN), load(P_RELEASE)),
            output_lin: 10f32.powf(load(P_OUTPUT) / 20.0),
            reverse: load(P_REVERSE) >= 0.5,
            filter_mode: filter_mode_from_param(load(P_FILTER_TYPE)),
            cutoff_hz: cutoff_units_to_hz(load(P_CUTOFF)),
            resonance: load(P_RESO),
            env_to_cutoff_st: load(P_ENV_CUTOFF),
            vel_to_amp: load(P_VEL_AMP),
            vel_to_cutoff_st: load(P_VEL_CUTOFF),
        };

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            // Two channels straight off the iterator. This used to `.collect()`
            // into a Vec — one heap allocation per block on the audio thread,
            // in five instruments. `.collect()` does not look like `vec![`, so
            // the lexical rt_safety scan never saw it; the counting allocator
            // in sdsp-test-kit did.
            let mut writers = channel_pairs
                .into_iter()
                .filter_map(superduper_dsp_sdk::clap_helpers::output_slice);
            let (Some(out_l), Some(out_r)) = (writers.next(), writers.next()) else {
                continue;
            };
            let frames = out_l.len().min(out_r.len());

            if bypassed {
                for i in 0..frames { out_l[i] = 0.0; out_r[i] = 0.0; }
                continue;
            }

            for i in 0..frames {
                let mut l = 0.0_f32;
                let mut r = 0.0_f32;
                for v in self.voices.iter_mut() {
                    let (vl, vr) = v.process(params);
                    l += vl;
                    r += vr;
                }
                out_l[i] = l;
                out_r[i] = r;
                self.shared.scope.push((l + r) * 0.5);
            }
        }

        Ok(ProcessStatus::Continue)
    }
}

impl<'a> PluginAudioProcessor<'a> {
    fn trigger(&mut self, key: u8, velocity: f32, note_id: i32) {
        self.next_age = self.next_age.wrapping_add(1);
        let stamp = self.next_age;
        // Find an idle voice or steal the oldest.
        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let params = VoiceParams {
            host_sr: self.sample_rate,
            root_key: load(P_ROOT),
            tune_st: load(P_TUNE),
            fine_cents: load(P_FINE),
            loop_on: load(P_LOOP) >= 0.5,
            loop_start_frac: load(P_LOOP_START),
            loop_end_frac: load(P_LOOP_END),
            trim_start_frac: load(P_TRIM_START),
            trim_end_frac: load(P_TRIM_END),
            env: AdsrParams::adsr(self.sample_rate, load(P_ATTACK), load(P_DECAY), load(P_SUSTAIN), load(P_RELEASE)),
            output_lin: 10f32.powf(load(P_OUTPUT) / 20.0),
            reverse: load(P_REVERSE) >= 0.5,
            filter_mode: filter_mode_from_param(load(P_FILTER_TYPE)),
            cutoff_hz: cutoff_units_to_hz(load(P_CUTOFF)),
            resonance: load(P_RESO),
            env_to_cutoff_st: load(P_ENV_CUTOFF),
            vel_to_amp: load(P_VEL_AMP),
            vel_to_cutoff_st: load(P_VEL_CUTOFF),
        };
        let sample = Arc::clone(&self.shared.active_sample.lock());
        let slot_idx = superduper_synth_core::dsp_blocks::pick_voice_slot(&self.voices);
        self.voices[slot_idx].gate_on(key, velocity, note_id, stamp, sample, params);
    }

    fn release(&mut self, key: u8) {
        for v in self.voices.iter_mut() {
            if v.key == key { v.gate_off(); v.key = NOTE_FREE; }
        }
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 { if is_input { 0 } else { 1 } }
    fn get(&mut self, index: u32, is_input: bool, w: &mut AudioPortInfoWriter) {
        if is_input || index != 0 { return; }
        w.set(&AudioPortInfo {
            id: ClapId::new(0), name: b"Output", channel_count: 2,
            flags: AudioPortFlags::IS_MAIN, port_type: Some(AudioPortType::STEREO),
            in_place_pair: None,
        });
    }
}

impl PluginNotePortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 { if is_input { 1 } else { 0 } }
    fn get(&mut self, index: u32, is_input: bool, w: &mut NotePortInfoWriter) {
        if !is_input || index != 0 { return; }
        w.set(&NotePortInfo {
            id: ClapId::new(0), name: b"MIDI In",
            supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
            preferred_dialect: Some(NoteDialect::Clap),
        });
    }
}

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 { PARAMS.len() as u32 }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info(PARAMS, idx, info);
    }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        let i = id.get() as usize;
        self.shared.params.get(i).map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(&mut self, id: ClapId, v: f64, w: &mut ParamDisplayWriter) -> core::fmt::Result {
        use core::fmt::Write;
        let pid = id.get() as usize;
        if pid == P_LOOP {
            return write!(w, "{}", if v >= 0.5 { "On" } else { "Off" });
        }
        if pid == P_SAMPLE {
            // Show "Pack / file stem" instead of a number.
            let lib = self.shared.library.lock();
            if let Some(entry) = lib.get(v.round() as usize) {
                if let Some(stem) = entry.path.file_stem() {
                    return write!(w, "{} / {}", entry.pack, stem.to_string_lossy());
                }
            }
        }
        ParamDef::write_display(PARAMS, id, v, w)
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        ParamDef::parse_text(PARAMS, id, t)
    }
    fn flush(&mut self, ev: &InputEvents, _: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
        // Main-thread flush (transport stopped / idle track): the audio
        // thread may never run its request_callback, so honour a Sample-param
        // change right here too, off the RT path.
        maybe_load_pending_sample(&self.shared.inner);
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, ev: &InputEvents, _: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
    }
}

superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);

use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window as ClapGuiWindow,
};

impl PluginGuiImpl for PluginMainThread<'_> {
    fn is_api_supported(&mut self, c: GuiConfiguration) -> bool {
        if c.is_floating { return false; }
        c.api_type == GuiApiType::COCOA || c.api_type == GuiApiType::WIN32 || c.api_type == GuiApiType::X11
    }
    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        let api_type = if cfg!(target_os = "macos") { GuiApiType::COCOA }
        else if cfg!(target_os = "windows") { GuiApiType::WIN32 }
        else { GuiApiType::X11 };
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
            can_resize_horizontally: true, can_resize_vertically: true,
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
    fn show(&mut self) -> Result<(), PluginError> { Ok(()) }
    fn hide(&mut self) -> Result<(), PluginError> { Ok(()) }
    fn set_transient(&mut self, _: ClapGuiWindow) -> Result<(), PluginError> { Ok(()) }
}

pub struct SuperDuperSampler;

impl Plugin for SuperDuperSampler {
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

impl DefaultPluginFactory for SuperDuperSampler {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("co.superduperai.sampler", plugin_display_name!("SuperDuper Sampler"))
            .with_vendor("SuperDuperAI")
            .with_version(version_string!("0.2"))
            .with_description("Polyphonic WAV sampler — scans known folders, plays any one-shot with pitch + ADSR + loop")
            .with_features([INSTRUMENT, STEREO, SAMPLER])
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        let shared = PluginShared::new();
        // Scan folders eagerly so the GUI dropdown is populated the
        // moment the user opens the window. Decoding is lazy — we
        // don't load any audio until they pick something.
        let count = refresh_library(&shared.inner);
        slog!("Sampler new_shared: found {} samples in default folders", count);
        // Background decoder thread: watches the `Sample` param and loads the
        // file off the RT thread whenever it changes. This is what makes the
        // sampler reliably **headless-selectable** (host automation / MCP /
        // producer-pal), independent of whether the host services
        // request_callback/on_main_thread — REAPER routes param flushes for an
        // active plugin to the audio thread, where decoding is forbidden, so
        // the callback path alone never fired. Holds a `Weak` so the thread
        // exits on its own when this plugin instance is dropped. The decode
        // (file I/O + resample) happens here; only the finished Arc is swapped
        // into `active_sample` under a brief lock — the audio thread never
        // blocks on I/O.
        let weak = std::sync::Arc::downgrade(&shared.inner);
        let _ = std::thread::Builder::new()
            .name("sdsp-sampler-loader".into())
            .spawn(move || loop {
                match weak.upgrade() {
                    Some(inner) => {
                        maybe_load_pending_sample(&inner);
                        drop(inner);
                    }
                    None => break,
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            });
        Ok(shared)
    }
    fn new_main_thread<'a>(_host: HostMainThreadHandle<'a>, shared: &'a PluginShared)
        -> Result<PluginMainThread<'a>, PluginError>
    {
        Ok(PluginMainThread {
            shared, gui_handle: None, gui_resize: gui::new_resize_bridge(),
        })
    }
}

clack_export_entry!(SinglePluginEntry<SuperDuperSampler>);
