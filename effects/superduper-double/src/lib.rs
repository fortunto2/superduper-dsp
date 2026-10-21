//! SuperDuper Double — realistic vocal doubler (Revoice Doubletracker idea).
//!
//! DSP lives in `superduper_synth_core::doubler` (shared PSOLA heads over one
//! tracker); this file is CLAP plumbing: params, stereo I/O, the latency
//! extension, state (params + bus key), the bus publisher, and the GUI.

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod presets;

use atomic_float::AtomicF32;
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
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
use superduper_dsp_sdk::clap_helpers::{enum_text_to_value, enum_value_to_text, split_io, ParamDef};
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::bus_key::KeyEnvelope;
pub use superduper_synth_core::doubler::{DoubleMode, Doubler, DoublerParams, MAX_VOICES};
use superduper_synth_core::scale;

fn init_logging() {
    superduper_dsp_sdk::log::init("double");
}
use superduper_dsp_sdk::slog;

// ===========================================================================
// Parameter table — FROZEN once shipped (append only, never reorder).
// ===========================================================================

pub const PARAMS: &[ParamDef] = &[
    // 0 = Natural, 1 = Tuned (doubles snapped), 2 = Tuned Lead (lead snapped).
    ParamDef { id: 0, name: b"Mode",      min: 0.0,   max: 2.0,   default: 0.0,  unit: "" },
    ParamDef { id: 1, name: b"Voices",    min: 0.0,   max: 4.0,   default: 2.0,  unit: "" },
    // Max lag behind the lead; each double wanders 0.3–1 × this.
    ParamDef { id: 2, name: b"Timing",    min: 0.0,   max: 80.0,  default: 30.0, unit: "ms" },
    ParamDef { id: 3, name: b"Pitch Var", min: 0.0,   max: 50.0,  default: 8.0,  unit: "ct" },
    ParamDef { id: 4, name: b"Formant",   min: -4.0,  max: 4.0,   default: 0.0,  unit: "st" },
    ParamDef { id: 5, name: b"Spread",    min: 0.0,   max: 1.0,   default: 0.8,  unit: "" },
    ParamDef { id: 6, name: b"Level",     min: -24.0, max: 6.0,   default: -3.0, unit: "dB" },
    ParamDef { id: 7, name: b"Mix",       min: 0.0,   max: 1.0,   default: 1.0,  unit: "" },
    ParamDef { id: 8, name: b"Dry",       min: 0.0,   max: 1.0,   default: 1.0,  unit: "" },
    ParamDef { id: 9, name: b"Output",    min: -24.0, max: 12.0,  default: 0.0,  unit: "dB" },
    ParamDef { id: 10, name: b"Seed",     min: 1.0,   max: 32.0,  default: 1.0,  unit: "" },
    ParamDef { id: 11, name: b"Key",      min: 0.0,   max: 11.0,  default: 0.0,  unit: "" },
    ParamDef { id: 12, name: b"Scale",    min: 0.0,   max: (scale::NUM_SCALES - 1) as f64, default: 0.0, unit: "" },
    ParamDef { id: 13, name: b"Retune",   min: 0.0,   max: 500.0, default: 20.0, unit: "ms" },
    // Bus key: 0 = off. At 1 the doubles are silent while the key track is
    // quiet and full while it is loud — they swell with the chorus.
    ParamDef { id: 14, name: b"Key Open", min: 0.0,   max: 1.0,   default: 0.0,  unit: "" },
    ParamDef { id: 15, name: b"Key Range", min: 6.0,  max: 40.0,  default: 24.0, unit: "dB" },
];

pub const STEPPED_PARAMS: &[u32] = &[0, 1, 10, 11, 12];

pub const P_MODE: usize = 0;
pub const P_VOICES: usize = 1;
pub const P_TIMING: usize = 2;
pub const P_PITCH_VAR: usize = 3;
pub const P_FORMANT: usize = 4;
pub const P_SPREAD: usize = 5;
pub const P_LEVEL: usize = 6;
pub const P_MIX: usize = 7;
pub const P_DRY: usize = 8;
pub const P_OUTPUT: usize = 9;
pub const P_SEED: usize = 10;
pub const P_KEY: usize = 11;
pub const P_SCALE: usize = 12;
pub const P_RETUNE: usize = 13;
pub const P_KEY_OPEN: usize = 14;
pub const P_KEY_RANGE: usize = 15;

pub const MODE_NAMES: [&str; 3] = ["Natural", "Tuned", "Tuned Lead"];

#[inline]
fn db_to_lin(db: f32) -> f32 {
    if db == 0.0 {
        1.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Read the param atomics into the engine's parameter struct. `key_gain` is
/// supplied by the caller (it comes from the bus, not from a param).
pub fn params_from(values: &[AtomicF32], bypassed: bool, key_gain: f32) -> DoublerParams {
    let v = |i: usize| values[i].load(Ordering::Relaxed);
    DoublerParams {
        mode: DoubleMode::from_index(v(P_MODE)),
        voices: v(P_VOICES).round().clamp(0.0, MAX_VOICES as f32) as usize,
        timing_ms: v(P_TIMING),
        pitch_var_cents: v(P_PITCH_VAR),
        formant_st: v(P_FORMANT),
        spread: v(P_SPREAD),
        level: db_to_lin(v(P_LEVEL)),
        mix: v(P_MIX),
        dry: v(P_DRY),
        output: db_to_lin(v(P_OUTPUT)),
        seed: v(P_SEED).round().max(1.0) as u32,
        key: v(P_KEY).round().clamp(0.0, 11.0) as u8,
        scale_mask: scale::scale_mask(v(P_SCALE)),
        retune_ms: v(P_RETUNE),
        key_gain,
        bypassed,
    }
}

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
    pub active_preset: AtomicU32,
    pub latency_samples: AtomicU32,
    /// Live readouts for the GUI. Delay < 0 marks a silent voice.
    pub voice_delay_ms: [AtomicF32; MAX_VOICES],
    pub voice_cents: [AtomicF32; MAX_VOICES],
    pub detected_hz: AtomicF32,
    pub lead_corr_st: AtomicF32,
    pub key_gain: AtomicF32,
}

pub struct PluginShared {
    pub inner: SharedParams,
}

impl PluginShared {
    pub fn new() -> Self {
        let f = |v: f32| AtomicF32::new(v);
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| f(PARAMS[i].default as f32)),
                bypass: std::sync::atomic::AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                active_preset: AtomicU32::new(0),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(4096),
                latency_samples: AtomicU32::new(0),
                voice_delay_ms: std::array::from_fn(|_| f(-1.0)),
                voice_cents: std::array::from_fn(|_| f(0.0)),
                detected_hz: f(0.0),
                lead_corr_st: f(0.0),
                key_gain: f(1.0),
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
    track_watch: superduper_synth_core::gui::TrackInfoWatch<'a>,
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    dbl: Box<Doubler>,
    key: KeyEnvelope,
    sr: f32,
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
}

impl PluginAudioProcessor<'_> {
    /// Bus key → doubles gain for this block. Key Open 0 is exactly 1.0.
    fn key_gain(&mut self, frames: usize) -> f32 {
        let open = self.shared.params[P_KEY_OPEN].load(Ordering::Relaxed).clamp(0.0, 1.0);
        let range = self.shared.params[P_KEY_RANGE].load(Ordering::Relaxed);
        self.key.update(&self.shared.scope, open, range, self.sr);
        let mut f = 0.0;
        for _ in 0..frames.max(1) {
            f = self.key.tick();
        }
        if open == 0.0 {
            1.0
        } else {
            1.0 - open * (1.0 - f)
        }
    }

    fn publish_readouts(&self) {
        let s = self.shared;
        for i in 0..MAX_VOICES {
            let (d, c) = self.dbl.voice_readout(i).unwrap_or((-1.0, 0.0));
            s.voice_delay_ms[i].store(d, Ordering::Relaxed);
            s.voice_cents[i].store(c, Ordering::Relaxed);
        }
        s.detected_hz.store(self.dbl.tracked_hz(), Ordering::Relaxed);
        s.lead_corr_st.store(self.dbl.lead_correction_st(), Ordering::Relaxed);
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
        shared.scope.set_sample_rate(sr);
        let mut dbl = Box::new(Doubler::new(sr, audio_config.max_frames_count as usize));
        let p = params_from(&shared.params, false, 1.0);
        dbl.prime(&p);
        shared.latency_samples.store(dbl.latency_samples(), Ordering::Relaxed);
        slog!("activate sr={} latency={}", sr, dbl.latency_samples());
        Ok(Self { shared, dbl, key: KeyEnvelope::default(), sr })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let _denormals = superduper_dsp_sdk::denormals::Guard::new();
        apply_param_events(self.shared, events.input);
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

        let frames = audio.frames_count() as usize;
        let key_gain = self.key_gain(frames);
        self.shared.key_gain.store(key_gain, Ordering::Relaxed);
        let params = params_from(
            &self.shared.params,
            self.shared.bypass.load(Ordering::Relaxed),
            key_gain,
        );

        if let Some(mut pair) = audio.port_pair(0) {
            let Some(channel_pairs) = pair.channels()?.into_f32() else {
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
                Some(w) => self.dbl.process(read_l, read_r, write_l, w, &params),
                None => {
                    let empty: &mut [f32] = &mut [];
                    self.dbl.process(read_l, read_r, write_l, empty, &params);
                }
            }
            for &s in write_l.iter() {
                self.shared.scope.push(s);
            }
        }
        self.publish_readouts();
        Ok(ProcessStatus::Continue)
    }
}

// ===========================================================================
// CLAP audio ports — one stereo main in/out.
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
            // No in-place: split_io would otherwise alias input and output.
            in_place_pair: None,
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
            P_MODE => enum_value_to_text(&MODE_NAMES, value, writer),
            P_KEY => write!(writer, "{}", scale::KEY_NAMES[(value.round() as usize).min(11)]),
            P_SCALE => {
                let s = (value.round().max(0.0) as usize).min(scale::NUM_SCALES - 1);
                write!(writer, "{}", scale::SCALES[s].0)
            }
            _ => ParamDef::write_display(PARAMS, id, value, writer),
        }
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        let names: &[&str] = match id.get() as usize {
            P_MODE => &MODE_NAMES,
            P_KEY => &scale::KEY_NAMES,
            _ => &[],
        };
        if let Some(v) = enum_text_to_value(names, t) {
            return Some(v);
        }
        if id.get() as usize == P_SCALE {
            let names: Vec<&str> = scale::SCALES.iter().map(|s| s.0).collect();
            if let Some(v) = enum_text_to_value(&names, t) {
                return Some(v);
            }
        }
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

impl PluginGuiImpl for PluginMainThread<'_> {
    fn is_api_supported(&mut self, c: GuiConfiguration) -> bool {
        !c.is_floating
            && (c.api_type == GuiApiType::COCOA
                || c.api_type == GuiApiType::WIN32
                || c.api_type == GuiApiType::X11)
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
        self.gui_resize.0.store(s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH), Ordering::Relaxed);
        self.gui_resize.1.store(s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT), Ordering::Relaxed);
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

pub struct SuperDuperDouble;

impl Plugin for SuperDuperDouble {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::track_info::PluginTrackInfo>()
            .register::<clack_extensions::latency::PluginLatency>();
    }
}

impl DefaultPluginFactory for SuperDuperDouble {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("co.superduperai.double", plugin_display_name!("SuperDuper Double"))
            .with_vendor("SuperDuperAI")
            .with_version(version_string!("0.1"))
            .with_description(
                "Vocal doubler — 1-4 doubles drifting in time and pitch, Natural / Tuned / Tuned Lead",
            )
            .with_features([AUDIO_EFFECT, STEREO, PITCH_SHIFTER])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Double — build {} ({})", build_num!(), build_date!());
        let shared = PluginShared::new();
        superduper_synth_core::gui::spawn_bus_publisher_mcp(
            "SuperDuper Double",
            &shared.inner,
            |s| &s.scope,
            Some(superduper_synth_core::gui::ParamsAccess {
                defs: PARAMS,
                values: |s| &s.params,
                dirty: Some(|s| &s.dirty_params),
            }),
            None,
        );
        Ok(shared)
    }

    fn new_main_thread<'a>(
        host: HostMainThreadHandle<'a>,
        shared: &'a PluginShared,
    ) -> Result<PluginMainThread<'a>, PluginError> {
        let track_watch = superduper_synth_core::gui::TrackInfoWatch::new(host, &shared.scope);
        Ok(PluginMainThread {
            track_watch,
            shared,
            gui_handle: None,
            gui_resize: gui::new_resize_bridge(),
        })
    }
}

clack_export_entry!(SinglePluginEntry<SuperDuperDouble>);

impl clack_extensions::track_info::PluginTrackInfoImpl for PluginMainThread<'_> {
    fn changed(&self) {
        self.track_watch.refresh();
    }
}
