//! SuperDuper Harmonic Clean — pitch-locked harmonic comb denoiser for a
//! piezo / electric kubyz (jaw-harp). Keeps the harmonic vibration and the
//! fast plucks, rejects the between-harmonic contact rustle. See `dsp.rs` for
//! the DSP; this file is the CLAP plumbing (params, ports, GUI, state).

#![allow(clippy::missing_safety_doc)]

pub mod dsp;
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
use std::sync::atomic::Ordering;
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};

use crate::dsp::{HarmonicCleaner, HarmonicParams};

fn init_logging() {
    superduper_dsp_sdk::log::init("harmonic");
}
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Param table — 6 params
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    // Between-harmonic rejection depth. 0 = bypass, 1 = maximum noise cut.
    ParamDef { id: 0, name: b"Amount",    min: 0.0,   max: 1.0,  default: 0.7,  unit: ""   },
    // Keep-width around each harmonic. Low = narrow (aggressive, many taps),
    // high = wide (gentle, few taps).
    ParamDef { id: 1, name: b"Bandwidth", min: 0.0,   max: 1.0,  default: 0.5,  unit: ""   },
    // Attack preservation. 0 = comb stays full on plucks (smears), 1 = plucks
    // pass through fully clean.
    ParamDef { id: 2, name: b"Transient", min: 0.0,   max: 1.0,  default: 0.6,  unit: ""   },
    // Dry/Wet.
    ParamDef { id: 3, name: b"Mix",       min: 0.0,   max: 1.0,  default: 1.0,  unit: ""   },
    // Output trim.
    ParamDef { id: 4, name: b"Output",    min: -24.0, max: 24.0, default: 0.0,  unit: "dB" },
    // Lowest fundamental to lock (Hz) — guards octave-down tracker errors.
    ParamDef { id: 5, name: b"Range",     min: 40.0,  max: 200.0, default: 60.0, unit: "Hz" },
    // Comb combine: 0 = Median (echo-robust, default), 1 = Mean (classic avg).
    ParamDef { id: 6, name: b"Mode",      min: 0.0,   max: 1.0,  default: 0.0,  unit: ""   },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[6];

pub const P_AMOUNT: usize = 0;
pub const P_BANDWIDTH: usize = 1;
pub const P_TRANSIENT: usize = 2;
pub const P_MIX: usize = 3;
pub const P_OUTPUT: usize = 4;
pub const P_RANGE: usize = 5;
pub const P_MODE: usize = 6;

// ---------------------------------------------------------------------------
// Shared params
// ---------------------------------------------------------------------------

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: std::sync::atomic::AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub dirty_params: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_begin: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_end: [std::sync::atomic::AtomicBool; PARAMS.len()],
    /// Currently-selected preset index — persisted via simple_state.
    pub active_preset: std::sync::atomic::AtomicU32,
    /// Live detected fundamental (Hz) — audio thread → GUI readout.
    pub detected_f0: AtomicF32,
    /// Live mean comb depth (0..1) — audio thread → GUI reduction bar.
    pub reduction: AtomicF32,
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
                detected_f0: AtomicF32::new(0.0),
                reduction: AtomicF32::new(0.0),
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

// ---------------------------------------------------------------------------
// Audio processor
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    cleaner: HarmonicCleaner,
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
        cfg: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = cfg.sample_rate as f32;
        slog!("activate sr={}", sr);
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        let out_lin = 10f32.powf(load(P_OUTPUT) / 20.0);
        let mut cleaner = HarmonicCleaner::new(sr);
        cleaner.prime(load(P_AMOUNT), load(P_TRANSIENT), load(P_MIX), out_lin);
        Ok(Self { shared, cleaner })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // Flush denormals to zero — the comb's long delay taps can otherwise
        // grind on ~10⁻³⁸ floats. RAII restores host CSR on drop.
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

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let p = HarmonicParams {
            amount: load(P_AMOUNT),
            bandwidth: load(P_BANDWIDTH),
            transient: load(P_TRANSIENT),
            mix: load(P_MIX),
            output_lin: 10f32.powf(load(P_OUTPUT) / 20.0),
            range_hz: load(P_RANGE),
            mode: load(P_MODE).round() as u32,
            bypassed,
        };

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else {
                continue;
            };
            let mut iter = channel_pairs.into_iter();
            let Some(ch_l) = iter.next() else { continue };
            let ch_r = iter.next();
            use superduper_dsp_sdk::clap_helpers::split_io;
            let Some((l_read, l_write)) = split_io(ch_l) else {
                continue;
            };
            let r = ch_r.and_then(split_io);

            match r {
                Some((r_read, r_write)) => {
                    self.cleaner
                        .process_stereo(l_read, r_read, l_write, r_write, &p);
                    let n = l_write.len().min(r_write.len());
                    for i in 0..n {
                        self.shared.scope.push((l_write[i] + r_write[i]) * 0.5);
                    }
                }
                None => {
                    // Mono: modulate off L, write only L (empty out_r).
                    self.cleaner
                        .process_stereo(l_read, l_read, l_write, &mut [], &p);
                    for &s in l_write.iter() {
                        self.shared.scope.push(s);
                    }
                }
            }
        }

        // Publish the live readouts for the GUI.
        self.shared
            .detected_f0
            .store(self.cleaner.detected_f0(), Ordering::Relaxed);
        self.shared
            .reduction
            .store(self.cleaner.reduction(), Ordering::Relaxed);

        Ok(ProcessStatus::Continue)
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

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
        ParamDef::write_display(PARAMS, id, v, w)
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

// ---------------------------------------------------------------------------
// CLAP GUI extension
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct SuperDuperHarmonic;

impl Plugin for SuperDuperHarmonic {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>();
    }
}

impl DefaultPluginFactory for SuperDuperHarmonic {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.harmonic",
            plugin_display_name!("SuperDuper Harmonic"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Pitch-locked harmonic comb denoiser for piezo/electric kubyz")
        .with_features([AUDIO_EFFECT, STEREO, RESTORATION, FILTER])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Harmonic — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperHarmonic>);
