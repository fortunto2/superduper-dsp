//! SuperDuper Granular — a live granular cloud (our Emergence).
//!
//! The input streams into a circular capture buffer; a scheduler keeps spawning
//! short windowed grains that read from behind the write head, each with its own
//! pitch, pan, direction and window. **Freeze** stops the capture so the cloud
//! chews the last few seconds forever — one sung note becomes an endless pad.
//!
//! DSP lives in `synth_core::granular` (so iOS/live2play gets it too); this file
//! is CLAP plumbing.

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
use superduper_synth_core::dsp_blocks::{sync_division_hz, sync_division_label};
use superduper_synth_core::granular::{GrainParams, GranularCloud};

fn init_logging() {
    superduper_dsp_sdk::log::init("granular");
}
use superduper_dsp_sdk::slog;

/// Factory preset count — kept separate from `PRESETS.len()` because `PARAMS`
/// needs it for the Preset param max (reading the static there is a const-eval
/// cycle). `tests/dsp_smoke.rs` asserts the two agree.
pub const PRESET_COUNT: usize = 10;

// ===========================================================================
// Parameter table — FROZEN once shipped.
// ===========================================================================

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0,  name: b"Density",  min: 0.5,   max: 200.0, default: 20.0,  unit: "gr/s" },
    ParamDef { id: 1,  name: b"Size",     min: 5.0,   max: 500.0, default: 80.0,  unit: "ms"   },
    ParamDef { id: 2,  name: b"Spray",    min: 0.0,   max: 1.0,   default: 0.2,   unit: ""     },
    ParamDef { id: 3,  name: b"Position", min: 0.0,   max: 1.0,   default: 0.05,  unit: ""     },
    ParamDef { id: 4,  name: b"Pitch",    min: -24.0, max: 24.0,  default: 0.0,   unit: "st"   },
    ParamDef { id: 5,  name: b"Jitter",   min: 0.0,   max: 12.0,  default: 0.0,   unit: "st"   },
    ParamDef { id: 6,  name: b"Spread",   min: 0.0,   max: 1.0,   default: 0.5,   unit: ""     },
    ParamDef { id: 7,  name: b"Reverse",  min: 0.0,   max: 1.0,   default: 0.0,   unit: ""     },
    // Boolean: stop capturing and loop what's in the buffer.
    ParamDef { id: 8,  name: b"Freeze",   min: 0.0,   max: 1.0,   default: 0.0,   unit: ""     },
    ParamDef { id: 9,  name: b"Feedback", min: 0.0,   max: 0.95,  default: 0.0,   unit: ""     },
    // 0 = Hann (smooth cloud), 1 = Tukey (sampler-ish), 2 = Perc (pointillist).
    ParamDef { id: 10, name: b"Shape",    min: 0.0,   max: 2.0,   default: 0.0,   unit: ""     },
    // Sync ties the spawn rate to the host grid instead of Density.
    ParamDef { id: 11, name: b"Sync",     min: 0.0,   max: 1.0,   default: 0.0,   unit: ""     },
    ParamDef { id: 12, name: b"Div",      min: 0.0,   max: 11.0,  default: 10.0,  unit: ""     },
    ParamDef { id: 13, name: b"Mix",      min: 0.0,   max: 1.0,   default: 1.0,   unit: ""     },
    ParamDef { id: 14, name: b"Output",   min: -24.0, max: 24.0,  default: 0.0,   unit: "dB"   },
    ParamDef { id: 15, name: b"Preset",   min: 0.0,   max: (PRESET_COUNT - 1) as f64, default: 0.0, unit: "" },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[8, 11, 12, 15];

pub const P_DENSITY: usize = 0;
pub const P_SIZE: usize = 1;
pub const P_SPRAY: usize = 2;
pub const P_POSITION: usize = 3;
pub const P_PITCH: usize = 4;
pub const P_JITTER: usize = 5;
pub const P_SPREAD: usize = 6;
pub const P_REVERSE: usize = 7;
pub const P_FREEZE: usize = 8;
pub const P_FEEDBACK: usize = 9;
pub const P_SHAPE: usize = 10;
pub const P_SYNC: usize = 11;
pub const P_DIV: usize = 12;
pub const P_MIX: usize = 13;
pub const P_OUTPUT: usize = 14;
pub const P_PRESET: usize = 15;

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
    /// Grains currently sounding — the cloud-density readout.
    pub live_grains: AtomicU32,
    /// Capture write head 0..1 — drawn as the moving line on the buffer strip.
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
                live_grains: AtomicU32::new(0),
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
    cloud: Box<GranularCloud>,
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
    ///   CC 64 Sustain    → Freeze  (a pedal freezing the cloud is the point)
    ///   CC 74 Brightness → Density
    ///   CC 71 Resonance  → Size
    ///   CC 73            → Feedback
    ///   CC 76            → Jitter
    ///   CC 1  ModWheel   → Position
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
                        74 => map_to(P_DENSITY),
                        71 => map_to(P_SIZE),
                        73 => map_to(P_FEEDBACK),
                        76 => map_to(P_JITTER),
                        1 => map_to(P_POSITION),
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
        let _ = shared;
        Ok(Self {
            shared,
            host,
            cloud: Box::new(GranularCloud::new(sr)),
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
        // Synced: the grid division IS the spawn rate (a 1/16 division spawns a
        // grain every sixteenth → rhythmic stutter instead of a smooth cloud).
        let density = if load(P_SYNC) >= 0.5 {
            sync_division_hz(
                load(P_DIV).round() as u32,
                self.shared.host_bpm.load(Ordering::Relaxed),
            )
        } else {
            load(P_DENSITY)
        };
        let params = GrainParams {
            density,
            size_ms: load(P_SIZE),
            spray: load(P_SPRAY),
            position: load(P_POSITION),
            pitch_semi: load(P_PITCH),
            jitter_semi: load(P_JITTER),
            spread: load(P_SPREAD),
            reverse: load(P_REVERSE),
            freeze: load(P_FREEZE) >= 0.5,
            feedback: load(P_FEEDBACK),
            shape: load(P_SHAPE).round() as u32,
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
                Some(w) => self.cloud.process(read_l, read_r, write_l, w, &params),
                None => {
                    let empty: &mut [f32] = &mut [];
                    self.cloud.process(read_l, read_r, write_l, empty, &params)
                }
            }

            for &s in write_l.iter() {
                self.shared.scope.push(s);
            }
        }

        self.shared
            .live_grains
            .store(self.cloud.live_grains() as u32, Ordering::Relaxed);
        self.shared
            .write_phase
            .store(self.cloud.write_phase(), Ordering::Relaxed);

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
            P_SYNC => return write!(writer, "{}", if value < 0.5 { "Free" } else { "Sync" }),
            P_SHAPE => {
                let names = ["Hann", "Tukey", "Perc"];
                let i = (value.round().max(0.0) as usize).min(names.len() - 1);
                return write!(writer, "{}", names[i]);
            }
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

pub struct SuperDuperGranular;

impl Plugin for SuperDuperGranular {
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

impl DefaultPluginFactory for SuperDuperGranular {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.granular",
            plugin_display_name!("SuperDuper Granular"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Live granular cloud — grains, freeze, and feedback textures")
        .with_features([AUDIO_EFFECT, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Granular — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperGranular>);
