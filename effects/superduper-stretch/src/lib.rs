//! SuperDuper Stretch — extreme time-stretch smear (our PaulXStretch).
//!
//! Long-window FFT, magnitudes kept, **phases randomised**, overlap-added at a
//! bigger hop than they were read with. That is the whole PaulStretch trick, and
//! it is why an 8× stretch sounds glassy instead of metallic. `Tonal` blends the
//! random phase back toward the analysed one, so the same plugin covers both a
//! plain slow-down and a full ambient wash. **Freeze** stops the capture and
//! circles the last `Length` seconds forever — sing a note, freeze it, get an
//! endless pad.
//!
//! DSP lives in `synth_core::paulstretch` (so iOS/live2play gets it too); this
//! file is CLAP plumbing.

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod presets;

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
use superduper_synth_core::paulstretch::{PaulStretch, StretchParams, WINDOW_SIZES};

fn init_logging() {
    superduper_dsp_sdk::log::init("stretch");
}
use superduper_dsp_sdk::slog;

/// Factory preset count — kept separate from `PRESETS.len()` because `PARAMS`
/// needs it for the Preset param max (reading the static there is a const-eval
/// cycle). `tests/dsp_smoke.rs` asserts the two agree.
pub const PRESET_COUNT: usize = 8;

// ===========================================================================
// Parameter table — FROZEN once shipped.
// ===========================================================================

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Stretch", min: 1.0,   max: 50.0,  default: 8.0,  unit: "x"  },
    // Stepped index into WINDOW_SIZES — 4096…65536 (85 ms … 1.37 s @ 48 kHz).
    // Stepped rather than continuous so an FFT plan exists for every value and
    // changing it never allocates on the audio thread.
    ParamDef { id: 1, name: b"Window",  min: 0.0,   max: 4.0,   default: 2.0,  unit: ""   },
    // 0 = fully random phase (classic smear), 1 = keep the analysed phase.
    ParamDef { id: 2, name: b"Tonal",   min: 0.0,   max: 1.0,   default: 0.0,  unit: ""   },
    ParamDef { id: 3, name: b"Smooth",  min: 0.0,   max: 1.0,   default: 0.0,  unit: ""   },
    ParamDef { id: 4, name: b"Pitch",   min: -24.0, max: 24.0,  default: 0.0,  unit: "st" },
    ParamDef { id: 5, name: b"Freeze",  min: 0.0,   max: 1.0,   default: 0.0,  unit: ""   },
    ParamDef { id: 6, name: b"Length",  min: 0.25,  max: 12.0,  default: 6.0,  unit: "s"  },
    ParamDef { id: 7, name: b"Mix",     min: 0.0,   max: 1.0,   default: 1.0,  unit: ""   },
    ParamDef { id: 8, name: b"Output",  min: -24.0, max: 24.0,  default: 0.0,  unit: "dB" },
    ParamDef { id: 9, name: b"Preset",  min: 0.0,   max: (PRESET_COUNT - 1) as f64, default: 0.0, unit: "" },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[1, 5, 9];

pub const P_STRETCH: usize = 0;
pub const P_WINDOW: usize = 1;
pub const P_TONAL: usize = 2;
pub const P_SMOOTH: usize = 3;
pub const P_PITCH: usize = 4;
pub const P_FREEZE: usize = 5;
pub const P_LENGTH: usize = 6;
pub const P_MIX: usize = 7;
pub const P_OUTPUT: usize = 8;
pub const P_PRESET: usize = 9;

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
    pub host_bpm: AtomicF32,
    /// Live sample rate, published by `activate` — the Window readout is a
    /// duration, so it can't assume 48 kHz (a 44.1 kHz session would read 9 %
    /// short, a 96 kHz one 2x long).
    pub sample_rate: AtomicF32,
    /// Stretch read head 0..1 of the ring — the slow-crawling playhead.
    pub read_phase: AtomicF32,
    /// Capture write head 0..1 — parked when frozen.
    pub write_phase: AtomicF32,
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
                sample_rate: AtomicF32::new(48_000.0),
                read_phase: AtomicF32::new(0.0),
                write_phase: AtomicF32::new(0.0),
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

/// GUI helper — write a param and flag it for the host's automation lane.
pub fn write_param(shared: &SharedParamsInner, idx: usize, value: f32) {
    if let Some(atom) = shared.params.get(idx) {
        atom.store(value, Ordering::Relaxed);
        if let Some(flag) = shared.dirty_params.get(idx) {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

/// Apply a factory preset — every param marked dirty (lesson 21d).
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
    stretch: Box<PaulStretch>,
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
}

impl PluginAudioProcessor<'_> {
    /// Transport (host BPM) + expressive MIDI CC.
    ///
    /// CC writes skip the dirty flag (lesson 21b) so the plugin never echoes a
    /// CC back into the host's automation lane.
    ///
    ///   CC 64 Sustain    → Freeze  (catch a moment mid-phrase with your foot)
    ///   CC 74 Brightness → Stretch
    ///   CC 71 Resonance  → Smooth
    ///   CC 73            → Tonal
    ///   CC 1  ModWheel   → Pitch
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
                    let raw = data[2];
                    let frac = raw as f32 / 127.0;
                    let map_to = |idx: usize| {
                        let def = &PARAMS[idx];
                        let val = def.min as f32 + frac * (def.max - def.min) as f32;
                        self.shared.params[idx].store(val, Ordering::Relaxed);
                    };
                    match cc {
                        64 => self.shared.params[P_FREEZE]
                            .store(if raw >= 64 { 1.0 } else { 0.0 }, Ordering::Relaxed),
                        74 => map_to(P_STRETCH),
                        71 => map_to(P_SMOOTH),
                        73 => map_to(P_TONAL),
                        1 => map_to(P_PITCH),
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
        shared.sample_rate.store(sr, Ordering::Relaxed);
        let _ = shared;
        Ok(Self {
            shared,
            host,
            stretch: Box::new(PaulStretch::new(sr)),
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
        let params = StretchParams {
            stretch: load(P_STRETCH),
            window: (load(P_WINDOW).round().max(0.0) as usize).min(WINDOW_SIZES.len() - 1),
            tonal: load(P_TONAL),
            smooth: load(P_SMOOTH),
            pitch_semi: load(P_PITCH),
            freeze: load(P_FREEZE) >= 0.5,
            length_s: load(P_LENGTH),
            mix: load(P_MIX),
            output_lin: 10f32.powf(load(P_OUTPUT) / 20.0),
            bypassed: self.shared.bypass.load(Ordering::Relaxed),
        };

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

            match write_r {
                Some(w) => self.stretch.process(read_l, read_r, write_l, w, &params),
                None => {
                    let empty: &mut [f32] = &mut [];
                    self.stretch.process(read_l, read_r, write_l, empty, &params)
                }
            }

            for &s in write_l.iter() {
                self.shared.scope.push(s);
            }
        }

        self.shared
            .read_phase
            .store(self.stretch.read_phase(), Ordering::Relaxed);
        self.shared
            .write_phase
            .store(self.stretch.write_phase(), Ordering::Relaxed);

        Ok(ProcessStatus::Continue)
    }
}

// ===========================================================================
// CLAP audio ports — plain stereo in/out (the grains come from the input).
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

/// Note port exists purely to receive MIDI CC (pedal → Freeze, gestures → knobs).
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
            P_FREEZE => return write!(writer, "{}", if value < 0.5 { "Live" } else { "Frozen" }),
            P_WINDOW => {
                // Show the actual window length — "16384" means nothing musically,
                // "341 ms" tells you how smeared it will be.
                let i = (value.round().max(0.0) as usize).min(WINDOW_SIZES.len() - 1);
                let sr = self.shared.sample_rate.load(Ordering::Relaxed).max(8_000.0);
                let ms = WINDOW_SIZES[i] as f32 * 1000.0 / sr;
                return write!(writer, "{ms:.0} ms");
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

pub struct SuperDuperStretch;

impl Plugin for SuperDuperStretch {
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

impl DefaultPluginFactory for SuperDuperStretch {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.stretch",
            plugin_display_name!("SuperDuper Stretch"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Extreme time-stretch smear — PaulStretch, live or frozen")
        .with_features([AUDIO_EFFECT, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Stretch — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperStretch>);
