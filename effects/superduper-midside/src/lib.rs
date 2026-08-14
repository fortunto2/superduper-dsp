//! SuperDuper Mid/Side — stereo width + per-channel gain via the
//! L-R sum/difference matrix.
//!
//! Math (per sample):
//!   M = (L + R) * 0.5         ← centre / mono content
//!   S = (L - R) * 0.5         ← stereo difference / sides
//!   M_out = M * mid_gain
//!   S_out = S * side_gain * width
//!   L = M_out + S_out
//!   R = M_out - S_out
//!
//! - `Width` = 0.0 → mono (sides cancelled)
//! - `Width` = 1.0 → identity (with mid/side gains = 1.0)
//! - `Width` = 2.0 → double sides — wide / "expanded" stereo
//! - `Mid` / `Side` gains let you boost the centre (vocal/kick) or
//!   sides (reverb tail) independently for mastering-grade balance.
//!
//! Used as a single insert on the master bus or any stereo source.
//! For TRUE Mid/Side processing (i.e. EQ only the mid channel),
//! chain two instances: one in "Encode" mode at the top (turns L/R
//! into M/S), then your processor (stereo EQ → L processes mid, R
//! processes side), then this plugin in "Decode" mode at the end.

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
use clack_common::stream::{InputStream, OutputStream};
use clack_extensions::state::{PluginState, PluginStateImpl};

use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::SmoothedParam;

fn init_logging() { superduper_dsp_sdk::log::init("midside"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Param table
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    // Mode: 0 = Width/Process (L/R in → L/R out, manipulated via M/S),
    //       1 = Encode (L/R in → L=Mid, R=Side out, for inserting a
    //                   stereo processor between encode+decode),
    //       2 = Decode (L=Mid, R=Side in → L/R out)
    ParamDef { id: 0, name: b"Mode",     min: 0.0,  max: 2.0,  default: 0.0,  unit: "" },
    ParamDef { id: 1, name: b"Width",    min: 0.0,  max: 2.0,  default: 1.0,  unit: "" },
    ParamDef { id: 2, name: b"Mid Gain", min: -24.0, max: 24.0, default: 0.0, unit: "dB" },
    ParamDef { id: 3, name: b"Sid Gain", min: -24.0, max: 24.0, default: 0.0, unit: "dB" },
    ParamDef { id: 4, name: b"Output",   min: -24.0, max: 12.0, default: 0.0, unit: "dB" },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[0];

pub const P_MODE: usize = 0;
pub const P_WIDTH: usize = 1;
pub const P_MID: usize = 2;
pub const P_SIDE: usize = 3;
pub const P_OUTPUT: usize = 4;

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
    pub active_preset: std::sync::atomic::AtomicU32,
}

pub struct PluginShared { pub inner: SharedParams }

impl PluginShared {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: std::sync::atomic::AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
                active_preset: std::sync::atomic::AtomicU32::new(0),
            }),
        }
    }
    pub fn shared_handle(&self) -> SharedParams { std::sync::Arc::clone(&self.inner) }
}

impl Default for PluginShared { fn default() -> Self { Self::new() } }
impl std::ops::Deref for PluginShared {
    type Target = SharedParamsInner;
    fn deref(&self) -> &SharedParamsInner { &self.inner }
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
    smooth_width: SmoothedParam,
    smooth_mid: SmoothedParam,
    smooth_side: SmoothedParam,
    smooth_output: SmoothedParam,
    sample_rate: f32,
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
        Ok(Self {
            shared,
            smooth_width: SmoothedParam::new(load(P_WIDTH)),
            smooth_mid: SmoothedParam::new(load(P_MID)),
            smooth_side: SmoothedParam::new(load(P_SIDE)),
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
        apply_param_events(self.shared, events.input);
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(
            &self.shared.params, &self.shared.dirty_params, events.output);
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(
            &self.shared.gesture_begin, &self.shared.gesture_end, events.output);

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        let sr = self.sample_rate;
        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let mode = load(P_MODE).round() as u32;
        let width_t = load(P_WIDTH);
        let mid_db_t = load(P_MID);
        let side_db_t = load(P_SIDE);
        let output_db_t = load(P_OUTPUT);

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            let mut iter = channel_pairs.into_iter();
            let Some(ch_l) = iter.next() else { continue };
            let Some(ch_r) = iter.next() else { continue };

            use superduper_dsp_sdk::clap_helpers::split_io;
            let Some((l_read, l_write)) = split_io(ch_l) else { continue };
            let Some((r_read, r_write)) = split_io(ch_r) else { continue };

            if bypassed {
                l_write.copy_from_slice(l_read);
                r_write.copy_from_slice(r_read);
                continue;
            }

            let n = l_read.len().min(r_read.len());
            for i in 0..n {
                let width = self.smooth_width.step(width_t, sr).max(0.0);
                let mid_lin = 10f32.powf(self.smooth_mid.step(mid_db_t, sr) / 20.0);
                let side_lin = 10f32.powf(self.smooth_side.step(side_db_t, sr) / 20.0);
                let out_lin = 10f32.powf(self.smooth_output.step(output_db_t, sr) / 20.0);

                let li = l_read[i];
                let ri = r_read[i];
                let (lo, ro) = match mode {
                    // Encode: L/R → L=M, R=S (no width / mid/side gain
                    // applied — those belong in the processor between
                    // encode and decode).
                    1 => {
                        let m = (li + ri) * 0.5;
                        let s = (li - ri) * 0.5;
                        (m, s)
                    }
                    // Decode: L=M, R=S → L/R. Same gains apply for
                    // symmetry with Encode at the chain edges.
                    2 => {
                        let m = li;
                        let s = ri;
                        (m + s, m - s)
                    }
                    // Default / Width: encode → scale → decode.
                    _ => {
                        let m = (li + ri) * 0.5 * mid_lin;
                        let s = (li - ri) * 0.5 * side_lin * width;
                        (m + s, m - s)
                    }
                };
                l_write[i] = lo * out_lin;
                r_write[i] = ro * out_lin;
                self.shared.scope.push((lo + ro) * 0.5);
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 { if is_input { 1 } else { 1 } }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index != 0 { return; }
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
    fn count(&mut self) -> u32 { PARAMS.len() as u32 }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info_stepped(PARAMS, idx, info, STEPPED_PARAMS);
    }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        let i = id.get() as usize;
        self.shared.params.get(i).map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(&mut self, id: ClapId, v: f64, w: &mut ParamDisplayWriter) -> core::fmt::Result {
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
        if c.is_floating { return false; }
        c.api_type == GuiApiType::COCOA || c.api_type == GuiApiType::WIN32 || c.api_type == GuiApiType::X11
    }
    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        let api_type = if cfg!(target_os = "macos") { GuiApiType::COCOA }
            else if cfg!(target_os = "windows") { GuiApiType::WIN32 } else { GuiApiType::X11 };
        Some(GuiConfiguration { api_type, is_floating: false })
    }
    fn create(&mut self, _: GuiConfiguration) -> Result<(), PluginError> { Ok(()) }
    fn destroy(&mut self) { self.gui_handle = None; }
    fn set_scale(&mut self, _: f64) -> Result<(), PluginError> { Ok(()) }
    fn get_size(&mut self) -> Option<GuiSize> {
        Some(GuiSize {
            width: self.gui_resize.0.load(AtomicOrdering::Relaxed),
            height: self.gui_resize.1.load(AtomicOrdering::Relaxed),
        })
    }
    fn can_resize(&mut self) -> bool { true }
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
    fn set_transient(&mut self, _: ClapGuiWindow) -> Result<(), PluginError> { Ok(()) }
    fn show(&mut self) -> Result<(), PluginError> { Ok(()) }
    fn hide(&mut self) -> Result<(), PluginError> { Ok(()) }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct SuperDuperMidSide;

impl Plugin for SuperDuperMidSide {
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

impl DefaultPluginFactory for SuperDuperMidSide {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.midside",
            plugin_display_name!("SuperDuper Mid/Side"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Stereo width + mid/side gain via L-R sum/difference matrix")
        .with_features([AUDIO_EFFECT, STEREO, UTILITY])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: MidSide — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperMidSide>);
