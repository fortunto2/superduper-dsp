//! SuperDuper Pitch — manual pitch + independent formant shifter (TD-PSOLA).
//!
//! DSP lives in `dsp.rs`; this file is CLAP plumbing: params, stereo audio
//! ports, the latency extension (PSOLA look-behind), state, and the egui GUI.

#![allow(clippy::missing_safety_doc)]

pub mod dsp;
pub mod gui;
pub mod keydetect;
pub mod presets;
pub mod pvoc;

pub use dsp::{PitchParams, PitchShifter};
pub use keydetect::KeyDetector;
pub use pvoc::PhaseVocoder;

/// `Mode` enum values.
pub const MODE_VOICE: u32 = 0;
pub const MODE_TRACK: u32 = 1;

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
use superduper_dsp_sdk::clap_helpers::{split_io, ParamDef};
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};

fn init_logging() {
    superduper_dsp_sdk::log::init("pitch");
}
use superduper_dsp_sdk::slog;

// ===========================================================================
// Parameter table — FROZEN once shipped.
// ===========================================================================

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Pitch",   min: -24.0, max: 24.0, default: 0.0, unit: "st" },
    ParamDef { id: 1, name: b"Formant", min: -12.0, max: 12.0, default: 0.0, unit: "st" },
    ParamDef { id: 2, name: b"Mix",     min: 0.0,   max: 1.0,  default: 1.0, unit: ""   },
    ParamDef { id: 3, name: b"Output",  min: -24.0, max: 24.0, default: 0.0, unit: "dB" },
    // 0 = Voice (TD-PSOLA, mono voice, best quality + independent formant),
    // 1 = Track (phase vocoder, transposes polyphony / whole mixes).
    ParamDef { id: 4, name: b"Mode",    min: 0.0,   max: 1.0,  default: 0.0, unit: ""   },
    // Target key for Match: 0 = None, 1..24 = C major..B minor (key index + 1).
    ParamDef { id: 5, name: b"Target",  min: 0.0,   max: 24.0, default: 0.0, unit: ""   },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[4, 5];

pub const P_PITCH: usize = 0;
pub const P_FORMANT: usize = 1;
pub const P_MIX: usize = 2;
pub const P_OUTPUT: usize = 3;
pub const P_MODE: usize = 4;
pub const P_TARGET_KEY: usize = 5;

// ===========================================================================
// Shared params (Arc so the egui thread can clone a handle).
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
    /// Detected key (0..23, or 24 = none) + confidence, published by the audio
    /// thread for the GUI (and the Match action).
    pub key_index: AtomicU32,
    pub key_conf: AtomicF32,
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
                key_index: AtomicU32::new(keydetect::KEY_NONE as u32),
                key_conf: AtomicF32::new(0.0),
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

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    /// Voice mode — TD-PSOLA (mono voice, independent formant).
    shifter: Box<PitchShifter>,
    /// Track mode — phase vocoder (polyphony / whole mixes).
    pvoc: Box<PhaseVocoder>,
    /// Musical key detector (runs in both modes).
    keydet: Box<KeyDetector>,
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
        // Both engines report the SAME latency so switching Mode never changes
        // the host-reported PDC — pad each up to the larger of the two needs.
        let fixed = PitchShifter::natural_latency(sr).max(pvoc::LATENCY);
        let mut shifter = Box::new(PitchShifter::with_latency(sr, max_frames, fixed));
        let pvoc = Box::new(PhaseVocoder::new(sr, fixed));
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        let out_lin = 10f32.powf(load(P_OUTPUT) / 20.0);
        shifter.prime(load(P_MIX), out_lin);
        let latency = shifter.latency_samples().max(pvoc.latency() as u32);
        shared.latency_samples.store(latency, Ordering::Relaxed);
        let keydet = Box::new(KeyDetector::new(sr));
        slog!("activate: sr={} latency={}", sr, latency);
        Ok(Self { shared, shifter, pvoc, keydet })
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

        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let params = PitchParams {
            pitch_st: load(P_PITCH),
            formant_st: load(P_FORMANT),
            mix: load(P_MIX),
            output_lin: 10f32.powf(load(P_OUTPUT) / 20.0),
            bypassed: self.shared.bypass.load(Ordering::Relaxed),
        };
        let track = load(P_MODE).round() as u32 == MODE_TRACK;

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else {
                continue;
            };
            let mut iter = channel_pairs.into_iter();
            let Some(ch_l) = iter.next() else { continue };
            let ch_r = iter.next();

            let Some((read_l, write_l)) = split_io(ch_l) else { continue };
            let (read_r, write_r): (&[f32], Option<&mut [f32]>) = match ch_r {
                Some(c) => match split_io(c) {
                    Some((r, w)) => (r, Some(w)),
                    None => (read_l, None),
                },
                None => (read_l, None),
            };

            // Feed the key detector from the (mono-summed) input — runs in both
            // modes so the GUI always shows the incoming key.
            let kn = read_l.len().min(read_r.len());
            for i in 0..kn {
                self.keydet.push((read_l[i] + read_r[i]) * 0.5);
            }

            let empty: &mut [f32] = &mut [];
            match (track, write_r) {
                (true, Some(w)) => self.pvoc.process(read_l, read_r, write_l, w, &params),
                (true, None) => self.pvoc.process(read_l, read_r, write_l, empty, &params),
                (false, Some(w)) => self.shifter.process(read_l, read_r, write_l, w, &params),
                (false, None) => self.shifter.process(read_l, read_r, write_l, empty, &params),
            }
            for &s in write_l.iter() {
                self.shared.scope.push(s);
            }
        }

        self.shared
            .key_index
            .store(self.keydet.key() as u32, Ordering::Relaxed);
        self.shared.key_conf.store(self.keydet.confidence(), Ordering::Relaxed);

        Ok(ProcessStatus::Continue)
    }
}

// ===========================================================================
// CLAP audio ports — plain stereo in/out.
// ===========================================================================

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 {
        1
    }
    fn get(&mut self, _index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: if is_input { b"Input" } else { b"Output" },
            channel_count: 2,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            // No in-place — see the note in the sibling plugins: it existed
            // only to let split_io alias one buffer as both input and output.
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
        if id.get() as usize == P_MODE {
            return write!(writer, "{}", if value < 0.5 { "Voice" } else { "Track" });
        }
        if id.get() as usize == P_TARGET_KEY {
            let v = value.round() as usize;
            return if v == 0 {
                write!(writer, "None")
            } else {
                write!(writer, "{}", keydetect::key_name(v - 1))
            };
        }
        ParamDef::write_display(PARAMS, id, value, writer)
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

pub struct SuperDuperPitch;

impl Plugin for SuperDuperPitch {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::latency::PluginLatency>();
    }
}

impl DefaultPluginFactory for SuperDuperPitch {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.pitch",
            plugin_display_name!("SuperDuper Pitch"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Manual pitch shifter with independent formant control (TD-PSOLA)")
        .with_features([AUDIO_EFFECT, STEREO, PITCH_SHIFTER])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Pitch — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperPitch>);
