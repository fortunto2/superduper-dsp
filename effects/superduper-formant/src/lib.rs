//! SuperDuper Formant — vowel resonators as an effect.
//!
//! Three band-pass formants (F1/F2/F3) imposed on any input, articulated three
//! ways: by hand on the vowel pad (**Manual**), by a tracked voice on the
//! sidechain (**Follow**), or by a moving trajectory (**Motion**). The DSP lives
//! in `dsp.rs` and `synth_core::{formant, formant_track}` (so iOS/live2play gets
//! it too); this file is CLAP plumbing.
//!
//! The flagship use: sing into the sidechain over a kubyz drone. The drone
//! speaks your vowels, and when you stop singing the last vowel *stays* — the
//! voice hands the phrase over to the instrument instead of cutting out.

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod presets;

// The DSP itself lives in synth-core so the mobile build can use it; re-exported
// here under the path every other plugin uses for its engine.
pub use superduper_synth_core::formant_fx as dsp;
pub use dsp::{FmtParams, FormantFx};

use atomic_float::AtomicF32;
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
use clack_extensions::state::PluginState;

use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use superduper_dsp_sdk::clap_helpers::{split_io, ParamDef};
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{sync_division_hz, sync_division_label};

fn init_logging() {
    superduper_dsp_sdk::log::init("formant");
}
use superduper_dsp_sdk::slog;

/// Number of factory presets. Declared separately from `PRESETS.len()` because
/// `PARAMS` needs it for the Preset param's max, and reading `PRESETS.len()`
/// there is a const-eval cycle (E0391). `presets.rs` static-asserts they match.
pub const PRESET_COUNT: usize = 13;

// ===========================================================================
// Parameter table — FROZEN once shipped (REAPER caches the layout per slot).
// ===========================================================================

pub const PARAMS: &[ParamDef] = &[
    // Formant centres. Ranges cover male + female tracts with headroom; the
    // vowel pad maps F2 to X and F1 to Y over exactly these spans.
    ParamDef { id: 0,  name: b"F1",      min: 200.0,  max: 1200.0, default: 700.0,  unit: "Hz" },
    ParamDef { id: 1,  name: b"F2",      min: 600.0,  max: 3000.0, default: 1200.0, unit: "Hz" },
    ParamDef { id: 2,  name: b"F3",      min: 1500.0, max: 4200.0, default: 2600.0, unit: "Hz" },
    // Bandwidth scale — 1.0 = natural vowel Q, < 1 narrow/nasal, > 1 airy.
    ParamDef { id: 3,  name: b"Width",   min: 0.25,   max: 4.0,    default: 1.0,    unit: ""   },
    ParamDef { id: 4,  name: b"Shift",   min: -12.0,  max: 12.0,   default: 0.0,    unit: "st" },
    // 0 = Manual (pad), 1 = Follow (sidechain voice), 2 = Motion (trajectory).
    ParamDef { id: 5,  name: b"Mode",    min: 0.0,    max: 2.0,    default: 0.0,    unit: ""   },
    ParamDef { id: 6,  name: b"Follow",  min: 0.0,    max: 1.0,    default: 1.0,    unit: ""   },
    ParamDef { id: 7,  name: b"Glide",   min: 2.0,    max: 500.0,  default: 40.0,   unit: "ms" },
    // 0 = Circle, 1 = Sine, 2 = Figure-8, 3 = Triangle, 4 = Line.
    ParamDef { id: 8,  name: b"Path",    min: 0.0,    max: 4.0,    default: 0.0,    unit: ""   },
    ParamDef { id: 9,  name: b"Rate",    min: 0.05,   max: 8.0,    default: 0.5,    unit: "Hz" },
    ParamDef { id: 10, name: b"Sync",    min: 0.0,    max: 1.0,    default: 0.0,    unit: ""   },
    ParamDef { id: 11, name: b"Div",     min: 0.0,    max: 11.0,   default: 6.0,    unit: ""   },
    ParamDef { id: 12, name: b"Depth",   min: 0.0,    max: 1.0,    default: 0.5,    unit: ""   },
    ParamDef { id: 13, name: b"Stereo",  min: 0.0,    max: 1.0,    default: 0.0,    unit: ""   },
    ParamDef { id: 14, name: b"Drive",   min: 0.0,    max: 1.0,    default: 0.0,    unit: ""   },
    ParamDef { id: 15, name: b"Mix",     min: 0.0,    max: 1.0,    default: 1.0,    unit: ""   },
    ParamDef { id: 16, name: b"Output",  min: -24.0,  max: 24.0,   default: 0.0,    unit: "dB" },
    // Stepped preset selector — lets a host / MCP agent recall a vowel or the
    // whole Follow patch without touching the GUI.
    ParamDef { id: 17, name: b"Preset",  min: 0.0,    max: (PRESET_COUNT - 1) as f64, default: 0.0, unit: "" },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[5, 8, 10, 11, 17];

pub const P_F1: usize = 0;
pub const P_F2: usize = 1;
pub const P_F3: usize = 2;
pub const P_WIDTH: usize = 3;
pub const P_SHIFT: usize = 4;
pub const P_MODE: usize = 5;
pub const P_FOLLOW: usize = 6;
pub const P_GLIDE: usize = 7;
pub const P_PATH: usize = 8;
pub const P_RATE: usize = 9;
pub const P_SYNC: usize = 10;
pub const P_DIV: usize = 11;
pub const P_DEPTH: usize = 12;
pub const P_STEREO: usize = 13;
pub const P_DRIVE: usize = 14;
pub const P_MIX: usize = 15;
pub const P_OUTPUT: usize = 16;
pub const P_PRESET: usize = 17;

// ===========================================================================
// Shared state
// ===========================================================================

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: AtomicBool,
    pub dirty_params: [AtomicBool; PARAMS.len()],
    pub gesture_begin: [AtomicBool; PARAMS.len()],
    pub gesture_end: [AtomicBool; PARAMS.len()],
    pub active_preset: AtomicU32,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    /// Host tempo, cached from Transport events for the synced Motion rate.
    pub host_bpm: AtomicF32,
    /// Formants actually in use on the last processed sample — the GUI draws
    /// the live cursor from these (Follow / Motion move them, not the params).
    pub live_f: [AtomicF32; 3],
    /// Motion LFO phase, for the trajectory cursor.
    pub motion_phase: AtomicF32,
    /// Sidechain level + gate state, so the user can see whether the tracker
    /// is listening or frozen.
    pub track_level_db: AtomicF32,
    pub track_active: AtomicBool,
}

pub struct PluginShared {
    pub inner: SharedParams,
}

impl PluginShared {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| AtomicBool::new(false)),
                active_preset: AtomicU32::new(0),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
                host_bpm: AtomicF32::new(120.0),
                live_f: [
                    AtomicF32::new(700.0),
                    AtomicF32::new(1200.0),
                    AtomicF32::new(2600.0),
                ],
                motion_phase: AtomicF32::new(0.0),
                track_level_db: AtomicF32::new(-120.0),
                track_active: AtomicBool::new(false),
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

/// GUI helper — write a param and flag it so the host records the move into its
/// automation lane (lesson 21a).
pub fn write_param(shared: &SharedParamsInner, idx: usize, value: f32) {
    if let Some(atom) = shared.params.get(idx) {
        atom.store(value, Ordering::Relaxed);
        if let Some(flag) = shared.dirty_params.get(idx) {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

/// Apply a factory preset. Marks **every** param dirty (lesson 21d) so a
/// recorded preset switch survives playback.
pub fn apply_preset(shared: &SharedParamsInner, preset: &presets::Preset) {
    for (i, v) in preset.values.iter().enumerate() {
        if let Some(atom) = shared.params.get(i) {
            atom.store(*v, Ordering::Relaxed);
        }
        if let Some(flag) = shared.dirty_params.get(i) {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

/// Recall preset `idx` and record it as active so `preset_recall_target` stops
/// re-firing. Main thread only.
pub fn apply_preset_idx(shared: &SharedParamsInner, idx: usize) {
    let Some(preset) = presets::PRESETS.get(idx) else { return };
    apply_preset(shared, preset);
    if let Some(atom) = shared.params.get(P_PRESET) {
        atom.store(idx as f32, Ordering::Relaxed);
    }
    shared.active_preset.store(idx as u32, Ordering::Relaxed);
}

// ===========================================================================
// Main thread / audio processor
// ===========================================================================

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {
    /// Preset recall runs here: the audio thread only requested the callback.
    fn on_main_thread(&mut self) {
        if let Some(idx) = superduper_dsp_sdk::clap_helpers::preset_recall_target(
            self.shared.params[P_PRESET].load(Ordering::Relaxed),
            &self.shared.active_preset,
            presets::PRESETS.len(),
        ) {
            apply_preset_idx(&self.shared.inner, idx);
        }
    }
}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    host: HostAudioProcessorHandle<'a>,
    fx: Box<FormantFx>,
    /// Pre-allocated sidechain (voice) scratch — never allocate on the audio thread.
    sc_l: Box<[f32]>,
    sc_r: Box<[f32]>,
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
}

impl PluginAudioProcessor<'_> {
    /// Transport (host BPM) + expressive MIDI CC.
    ///
    /// CC writes go straight into the atomics **without** raising the dirty flag
    /// (lesson 21b) — otherwise every CC would be echoed back as a
    /// ParamValueEvent, re-recorded by the host, and replayed into this handler.
    ///
    /// The map matches the live2play gesture defaults so the phone's gestures
    /// articulate the formants directly:
    ///   CC 1  ModWheel   → F1     (jaw open / close)
    ///   CC 74 Brightness → F2     (hands apart — front / back vowel)
    ///   CC 71 Resonance  → Width
    ///   CC 73 Attack     → Drive
    ///   CC 76 Vib Rate   → Depth  (motion amount)
    fn handle_events(&mut self, events: &InputEvents) {
        for event in events {
            let Some(core) = event.as_core_event() else { continue };
            match core {
                CoreEventSpace::Transport(t) => {
                    self.shared.host_bpm.store(t.tempo as f32, Ordering::Relaxed);
                }
                CoreEventSpace::Midi(m) => {
                    let data = m.data();
                    if data[0] & 0xf0 != 0xb0 {
                        continue;
                    }
                    let cc = data[1];
                    let frac = data[2] as f32 / 127.0;
                    let map_to = |idx: usize| {
                        let def = &PARAMS[idx];
                        let val = def.min as f32 + frac * (def.max - def.min) as f32;
                        self.shared.params[idx].store(val, Ordering::Relaxed);
                    };
                    match cc {
                        1 => map_to(P_F1),
                        74 => map_to(P_F2),
                        71 => map_to(P_WIDTH),
                        73 => map_to(P_DRIVE),
                        76 => map_to(P_DEPTH),
                        _ => {}
                    }
                }
                _ => {}
            }
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
        slog!("activate: sr={}", sr);
        let max_frames = audio_config.max_frames_count as usize;
        let mut fx = Box::new(FormantFx::new(sr));
        // Snap the glide state to whatever the host loaded, so the first block
        // isn't a sweep up from the table defaults.
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        fx.prime(&FmtParams {
            f1: load(P_F1),
            f2: load(P_F2),
            f3: load(P_F3),
            ..FmtParams::default()
        });
        Ok(Self {
            shared,
            host,
            fx,
            sc_l: vec![0.0; max_frames].into_boxed_slice(),
            sc_r: vec![0.0; max_frames].into_boxed_slice(),
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // High-Q band-passes spin up denormals on release tails.
        let _denormals = superduper_dsp_sdk::denormals::Guard::new();
        apply_param_events(self.shared, events.input);
        self.handle_events(events.input);
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
        // Preset moved (GUI combo, host automation, or MCP)? The recall
        // allocates, so it has to happen on the main thread.
        if superduper_dsp_sdk::clap_helpers::preset_recall_target(
            self.shared.params[P_PRESET].load(Ordering::Relaxed),
            &self.shared.active_preset,
            presets::PRESETS.len(),
        )
        .is_some()
        {
            self.host.shared().request_callback();
        }

        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let synced = load(P_SYNC) >= 0.5;
        let rate_hz = if synced {
            sync_division_hz(
                load(P_DIV).round() as u32,
                self.shared.host_bpm.load(Ordering::Relaxed),
            )
        } else {
            load(P_RATE)
        };
        let params = FmtParams {
            f1: load(P_F1),
            f2: load(P_F2),
            f3: load(P_F3),
            width: load(P_WIDTH),
            shift_semi: load(P_SHIFT),
            mode: load(P_MODE).round() as u32,
            follow: load(P_FOLLOW),
            glide_ms: load(P_GLIDE),
            path: load(P_PATH).round() as u32,
            rate_hz,
            depth: load(P_DEPTH),
            stereo: load(P_STEREO),
            drive: load(P_DRIVE),
            mix: load(P_MIX),
            output_lin: 10f32.powf(load(P_OUTPUT) / 20.0),
            bypassed: self.shared.bypass.load(Ordering::Relaxed),
        };

        // ---- Snapshot the voice sidechain (input port 1) -------------------
        let frames = audio.frames_count() as usize;
        let n_frames = frames.min(self.sc_l.len());
        self.sc_l[..n_frames].fill(0.0);
        self.sc_r[..n_frames].fill(0.0);
        if let Some(sc_port) = audio.input_port(1) {
            if let Some(chans) = sc_port.channels()?.into_f32() {
                if let Some(l) = chans.channel(0) {
                    let k = n_frames.min(l.len());
                    self.sc_l[..k].copy_from_slice(&l[..k]);
                }
                if let Some(r) = chans.channel(1) {
                    let k = n_frames.min(r.len());
                    self.sc_r[..k].copy_from_slice(&r[..k]);
                } else {
                    self.sc_r[..n_frames].copy_from_slice(&self.sc_l[..n_frames]);
                }
            }
        }

        // ---- Main port ----------------------------------------------------
        if let Some(mut main_pair) = audio.port_pair(0) {
            let Some(channel_pairs) = main_pair.channels()?.into_f32() else {
                return Ok(ProcessStatus::Continue);
            };
            let mut iter = channel_pairs.into_iter();
            let Some(ch_l) = iter.next() else {
                return Ok(ProcessStatus::Continue);
            };
            let ch_r = iter.next();

            let Some((read_l, write_l)) = split_io(ch_l) else {
                return Ok(ProcessStatus::Continue);
            };
            let (read_r, write_r): (&[f32], Option<&mut [f32]>) = match ch_r {
                Some(c) => match split_io(c) {
                    Some((r, w)) => (r, Some(w)),
                    None => (read_l, None),
                },
                None => (read_l, None),
            };

            let sc_l = &self.sc_l[..n_frames];
            let sc_r = &self.sc_r[..n_frames];
            match write_r {
                Some(w) => self
                    .fx
                    .process_stereo(read_l, read_r, write_l, w, sc_l, sc_r, &params),
                None => {
                    let empty: &mut [f32] = &mut [];
                    self.fx
                        .process_stereo(read_l, read_r, write_l, empty, sc_l, sc_r, &params)
                }
            }

            for &s in write_l.iter() {
                self.shared.scope.push(s);
            }
        }

        // ---- Publish live state for the GUI (once per block) ---------------
        let live = self.fx.current_formants();
        for k in 0..3 {
            self.shared.live_f[k].store(live[k], Ordering::Relaxed);
        }
        self.shared
            .motion_phase
            .store(self.fx.motion_phase(), Ordering::Relaxed);
        self.shared
            .track_level_db
            .store(self.fx.tracker_level_db(), Ordering::Relaxed);
        self.shared
            .track_active
            .store(self.fx.tracker_active(), Ordering::Relaxed);

        Ok(ProcessStatus::Continue)
    }
}

// ===========================================================================
// CLAP audio ports — main stereo I/O + a stereo voice sidechain.
// ===========================================================================

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input {
            2
        } else {
            1
        }
    }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        match (index, is_input) {
            (0, _) => writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: if is_input { b"Input" } else { b"Output" },
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                // No in-place: the host must hand us separate input and output
                // buffers. With in-place allowed, split_io had to return a
                // &[f32] and a &mut [f32] over the SAME memory to keep the
                // "read x[i], write y[i]" style working — two noalias slices
                // aliasing each other, which is undefined behaviour whatever
                // the access order. The host's own copy costs the same as the
                // scratch buffer we would otherwise keep per plugin.
                in_place_pair: None,
            }),
            (1, true) => writer.set(&AudioPortInfo {
                id: ClapId::new(1),
                name: b"Voice",
                channel_count: 2,
                flags: AudioPortFlags::empty(),
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            }),
            _ => {}
        }
    }
}

/// A note-input port so the plugin receives MIDI CC (gesture control of the
/// formants). No notes are played — this is purely the CC path.
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
            name: b"CC Control",
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
        match id.get() as usize {
            P_MODE => {
                let names = ["Manual", "Follow", "Motion"];
                let i = (value.round().max(0.0) as usize).min(names.len() - 1);
                return write!(writer, "{}", names[i]);
            }
            P_PATH => {
                let names = ["Circle", "Sine", "Figure-8", "Triangle", "Line"];
                let i = (value.round().max(0.0) as usize).min(names.len() - 1);
                return write!(writer, "{}", names[i]);
            }
            P_SYNC => return write!(writer, "{}", if value < 0.5 { "Free" } else { "Sync" }),
            P_DIV => {
                return write!(writer, "{}", sync_division_label(value.round().max(0.0) as u32))
            }
            P_PRESET => {
                if let Some(r) = superduper_dsp_sdk::clap_helpers::preset_value_to_text(
                    |i| presets::PRESETS.get(i).map(|p| p.name),
                    value,
                    writer,
                ) {
                    return r;
                }
            }
            _ => {}
        }
        ParamDef::write_display(PARAMS, id, value, writer)
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        if id.get() as usize == P_PRESET {
            if let Some(v) = superduper_dsp_sdk::clap_helpers::preset_text_to_value(
                presets::PRESETS.len(),
                |i| presets::PRESETS.get(i).map(|p| p.name),
                t,
            ) {
                return Some(v);
            }
        }
        ParamDef::parse_text(PARAMS, id, t)
    }
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        apply_param_events(self.shared, ev);
        // Mirror the audio-thread recall path: a host can move Preset while
        // transport is stopped, and then only flush runs.
        if let Some(idx) = superduper_dsp_sdk::clap_helpers::preset_recall_target(
            self.shared.params[P_PRESET].load(Ordering::Relaxed),
            &self.shared.active_preset,
            presets::PRESETS.len(),
        ) {
            apply_preset_idx(&self.shared.inner, idx);
        }
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        apply_param_events(self.shared, ev);
    }
}

superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);

// ===========================================================================
// CLAP GUI extension
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
        self.gui_resize
            .0
            .store(s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH), Ordering::Relaxed);
        self.gui_resize
            .1
            .store(s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT), Ordering::Relaxed);
        Ok(())
    }
    fn set_parent(&mut self, window: ClapGuiWindow) -> Result<(), PluginError> {
        slog!("gui::set_parent");
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
// Factory
// ===========================================================================

pub struct SuperDuperFormant;

impl Plugin for SuperDuperFormant {
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

impl DefaultPluginFactory for SuperDuperFormant {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.formant",
            plugin_display_name!("SuperDuper Formant"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Vowel formant filter — articulate any sound by hand, trajectory, or voice")
        .with_features([AUDIO_EFFECT, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Formant — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperFormant>);
