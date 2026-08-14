//! SuperDuper Tune — autotune / pitch correction (scale · MIDI · sidechain).
//!
//! DSP lives in `dsp.rs` (`Tune`) + `scale.rs`; this file is CLAP plumbing:
//! params, main stereo I/O + a stereo **sidechain** reference input, a note
//! input for the MIDI target, the latency extension (PSOLA look-behind), state,
//! and the egui GUI.

#![allow(clippy::missing_safety_doc)]

pub mod dsp;
pub mod gui;
pub mod presets;
pub mod scale;
pub mod wheel;

pub use dsp::{Tune, TuneParams, TARGET_MIDI, TARGET_SCALE, TARGET_SIDECHAIN};

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
use std::sync::atomic::{AtomicU32, Ordering};
use superduper_dsp_sdk::clap_helpers::{split_io, ParamDef};
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};

fn init_logging() {
    superduper_dsp_sdk::log::init("tune");
}
use superduper_dsp_sdk::slog;

// ===========================================================================
// Parameter table — FROZEN once shipped.
// ===========================================================================

pub const PARAMS: &[ParamDef] = &[
    // Key root (0 = C .. 11 = B) — used by the Scale target.
    ParamDef { id: 0, name: b"Key",     min: 0.0,   max: 11.0,  default: 0.0,  unit: "" },
    // Scale index into `scale::SCALES`.
    ParamDef { id: 1, name: b"Scale",   min: 0.0,   max: (scale::NUM_SCALES - 1) as f64, default: 1.0, unit: "" },
    // Target source: 0 = Scale, 1 = MIDI, 2 = Sidechain.
    ParamDef { id: 2, name: b"Target",  min: 0.0,   max: 2.0,   default: 0.0,  unit: "" },
    // Retune time — 0 = hard (T-Pain), larger = natural glide.
    ParamDef { id: 3, name: b"Retune",  min: 0.0,   max: 500.0, default: 0.0,  unit: "ms" },
    ParamDef { id: 4, name: b"Amount",  min: 0.0,   max: 1.0,   default: 1.0,  unit: "" },
    ParamDef { id: 5, name: b"Formant", min: -12.0, max: 12.0,  default: 0.0,  unit: "st" },
    ParamDef { id: 6, name: b"Mix",     min: 0.0,   max: 1.0,   default: 1.0,  unit: "" },
    ParamDef { id: 7, name: b"Output",  min: -24.0, max: 24.0,  default: 0.0,  unit: "dB" },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[2];

pub const P_KEY: usize = 0;
pub const P_SCALE: usize = 1;
pub const P_TARGET: usize = 2;
pub const P_RETUNE: usize = 3;
pub const P_AMOUNT: usize = 4;
pub const P_FORMANT: usize = 5;
pub const P_MIX: usize = 6;
pub const P_OUTPUT: usize = 7;

// ===========================================================================
// Shared params.
// ===========================================================================

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: std::sync::atomic::AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub dirty_params: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_begin: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_end: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub active_preset: std::sync::atomic::AtomicU32,
    /// PSOLA latency reported to the host for PDC. Set in `activate`.
    pub latency_samples: AtomicU32,
    /// Live readouts for the GUI (detected input pitch + applied correction).
    pub detected_hz: AtomicF32,
    pub correction_st: AtomicF32,
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
                latency_samples: AtomicU32::new(0),
                detected_hz: AtomicF32::new(0.0),
                correction_st: AtomicF32::new(0.0),
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

// ===========================================================================
// Main thread / audio processor.
// ===========================================================================

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

/// Monophonic MIDI-target note priority: last note pressed wins, falling back to
/// the previous still-held note on release.
const HELD_MAX: usize = 16;

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    tune: Box<Tune>,
    /// Sidechain reference scratch (input port 1), filled each block.
    sc_l: Box<[f32]>,
    sc_r: Box<[f32]>,
    /// Held-note stack for the MIDI target (top = most recent).
    held: [i16; HELD_MAX],
    n_held: usize,
}

impl PluginAudioProcessor<'_> {
    fn note_on(&mut self, key: u8) {
        let k = key as i16;
        if self.held[..self.n_held].iter().any(|&n| n == k) {
            return;
        }
        if self.n_held < HELD_MAX {
            self.held[self.n_held] = k;
            self.n_held += 1;
        }
    }
    fn note_off(&mut self, key: u8) {
        let k = key as i16;
        if let Some(idx) = self.held[..self.n_held].iter().position(|&n| n == k) {
            for i in idx..self.n_held - 1 {
                self.held[i] = self.held[i + 1];
            }
            self.n_held -= 1;
        }
    }
    fn all_notes_off(&mut self) {
        self.n_held = 0;
    }
    fn midi_target(&self) -> i16 {
        if self.n_held > 0 {
            self.held[self.n_held - 1]
        } else {
            -1
        }
    }

    fn handle_note_events(&mut self, events: &InputEvents) {
        for event in events {
            let Some(core) = event.as_core_event() else { continue };
            match core {
                CoreEventSpace::NoteOn(n) => {
                    if let Match::Specific(k) = n.key() {
                        self.note_on(k as u8);
                    }
                }
                CoreEventSpace::NoteOff(n) => match n.key() {
                    Match::Specific(k) => self.note_off(k as u8),
                    Match::All => self.all_notes_off(),
                },
                CoreEventSpace::NoteChoke(n) => match n.key() {
                    Match::Specific(k) => self.note_off(k as u8),
                    Match::All => self.all_notes_off(),
                },
                CoreEventSpace::Midi(m) => {
                    let data = m.data();
                    let status = data[0] & 0xf0;
                    let key = data[1];
                    let vel = data[2];
                    match status {
                        0x90 if vel > 0 => self.note_on(key),
                        0x80 | 0x90 => self.note_off(key),
                        0xb0 if key == 123 || key == 120 => self.all_notes_off(),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
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
        let max_frames = audio_config.max_frames_count as usize;
        let mut tune = Box::new(Tune::new(sr, max_frames));
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        let out_lin = 10f32.powf(load(P_OUTPUT) / 20.0);
        tune.prime(load(P_MIX), out_lin);
        shared
            .latency_samples
            .store(tune.latency_samples(), Ordering::Relaxed);
        slog!("activate: sr={} latency={}", sr, tune.latency_samples());
        Ok(Self {
            shared,
            tune,
            sc_l: vec![0.0; max_frames].into_boxed_slice(),
            sc_r: vec![0.0; max_frames].into_boxed_slice(),
            held: [-1; HELD_MAX],
            n_held: 0,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let _denormals = superduper_dsp_sdk::denormals::Guard::new();
        apply_param_events(self.shared, events.input);
        self.handle_note_events(events.input);
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

        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let scale_idx = (load(P_SCALE).round() as usize).min(scale::NUM_SCALES - 1);
        let params = TuneParams {
            key: load(P_KEY).round().clamp(0.0, 11.0) as u8,
            scale_mask: scale::SCALES[scale_idx].1,
            target: load(P_TARGET).round().clamp(0.0, 2.0) as u32,
            retune_ms: load(P_RETUNE),
            amount: load(P_AMOUNT),
            formant_st: load(P_FORMANT),
            mix: load(P_MIX),
            output_lin: 10f32.powf(load(P_OUTPUT) / 20.0),
            midi_note: self.midi_target(),
            bypassed: self.shared.bypass.load(Ordering::Relaxed),
        };

        // ---- Snapshot the sidechain reference (input port 1) ---------------
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

        // ---- Process the main port (index 0) -------------------------------
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
                Some(w) => self.tune.process(read_l, read_r, sc_l, sc_r, write_l, w, &params),
                None => {
                    let empty: &mut [f32] = &mut [];
                    self.tune.process(read_l, read_r, sc_l, sc_r, write_l, empty, &params);
                }
            }
            for &s in write_l.iter() {
                self.shared.scope.push(s);
            }
        }

        self.shared
            .detected_hz
            .store(self.tune.detected_hz(), Ordering::Relaxed);
        self.shared
            .correction_st
            .store(self.tune.correction_st(), Ordering::Relaxed);

        Ok(ProcessStatus::Continue)
    }
}

// ===========================================================================
// CLAP audio ports — main stereo I/O + a stereo sidechain reference input.
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
                in_place_pair: Some(ClapId::new(0)),
            }),
            (1, true) => writer.set(&AudioPortInfo {
                id: ClapId::new(1),
                name: b"Reference",
                channel_count: 2,
                flags: AudioPortFlags::empty(),
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            }),
            _ => {}
        }
    }
}

// ===========================================================================
// CLAP note ports — one input for the MIDI target.
// ===========================================================================

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
            name: b"Target Note",
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
    fn value_to_text(
        &mut self,
        id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        use core::fmt::Write;
        match id.get() as usize {
            P_KEY => {
                let k = (value.round() as usize).min(11);
                write!(writer, "{}", scale::KEY_NAMES[k])
            }
            P_SCALE => {
                let s = (value.round() as usize).min(scale::NUM_SCALES - 1);
                write!(writer, "{}", scale::SCALES[s].0)
            }
            P_TARGET => write!(
                writer,
                "{}",
                match value.round() as u32 {
                    TARGET_MIDI => "MIDI",
                    TARGET_SIDECHAIN => "Sidechain",
                    _ => "Scale",
                }
            ),
            _ => ParamDef::write_display(PARAMS, id, value, writer),
        }
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        ParamDef::parse_text(PARAMS, id, t)
    }
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        apply_param_events(self.shared, ev);
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        apply_param_events(self.shared, ev);
    }
}

superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);

impl clack_extensions::latency::PluginLatencyImpl for PluginMainThread<'_> {
    fn get(&mut self) -> u32 {
        self.shared.latency_samples.load(Ordering::Relaxed)
    }
}

// ===========================================================================
// CLAP GUI extension.
// ===========================================================================

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
        Some(GuiConfiguration { api_type, is_floating: false })
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

// ===========================================================================
// Factory.
// ===========================================================================

pub struct SuperDuperTune;

impl Plugin for SuperDuperTune {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::latency::PluginLatency>();
    }
}

impl DefaultPluginFactory for SuperDuperTune {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.tune",
            plugin_display_name!("SuperDuper Tune"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Autotune — pitch correction to scale / MIDI / sidechain with formant preservation")
        .with_features([AUDIO_EFFECT, STEREO, PITCH_SHIFTER])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Tune — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperTune>);
