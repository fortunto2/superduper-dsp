//! SuperDuper Saturator — analog-style warmth / drive.
//!
//! Three saturation curves (selectable via the Type param):
//!   - Tape (algebraic soft-clip, soft top)
//!   - Tube (asymmetric, strong 2nd harmonic)
//!   - Soft (tanh, classic clean limiter-like)
//!
//! Signal chain (per sample):
//!   in → DCblock → Drive (linear gain from dB knob) → saturate(curve)
//!      → Tilt (one-pole high-shelf, ±6 dB at ±1.0)
//!      → Output gain → Mix with dry
//!
//! Mono signal-path math; the same chain runs on each channel with its
//! own per-channel state (DcBlocker / Tilt). No cross-channel coupling.

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
use superduper_synth_core::dsp_blocks::{
    tanh_drive, tape_clip, tube_clip, DcBlocker, Oversampler2x, SmoothedParam, Tilt,
};

// ---------------------------------------------------------------------------
// Logging — file in ~/.superduper-dsp/saturator.log
// ---------------------------------------------------------------------------

fn init_logging() { superduper_dsp_sdk::log::init("saturator"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Param table
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Drive",  min: 0.0,   max: 36.0, default: 6.0,  unit: "dB" },
    // 0 = Tape, 1 = Tube, 2 = Soft (tanh)
    ParamDef { id: 1, name: b"Type",   min: 0.0,   max: 2.0,  default: 0.0,  unit: ""   },
    ParamDef { id: 2, name: b"Tone",   min: -1.0,  max: 1.0,  default: 0.0,  unit: ""   },
    ParamDef { id: 3, name: b"Output", min: -24.0, max: 12.0, default: 0.0,  unit: "dB" },
    ParamDef { id: 4, name: b"Mix",    min: 0.0,   max: 1.0,  default: 1.0,  unit: ""   },
    // Oversampling — 0 = off (cheap, aliasing at high drive), 1 = 2× (good
    // for most cases), 2 = 4× (mastering-grade, ≥80 dB aliasing rejection).
    ParamDef { id: 5, name: b"OS",     min: 0.0,   max: 2.0,  default: 1.0,  unit: ""   },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[1, 5];

pub const P_DRIVE: usize = 0;
pub const P_TYPE: usize = 1;
pub const P_TONE: usize = 2;
pub const P_OUTPUT: usize = 3;
pub const P_MIX: usize = 4;
pub const P_OS: usize = 5;

// ---------------------------------------------------------------------------
// Shared params (Arc pattern shared with the GUI thread)
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
    /// Currently-selected preset index — persisted via simple_state
    /// so the dropdown survives project reopens.
    pub active_preset: std::sync::atomic::AtomicU32,
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
            }),
        }
    }
    pub fn shared_handle(&self) -> SharedParams { std::sync::Arc::clone(&self.inner) }
}

impl Default for PluginShared {
    fn default() -> Self { Self::new() }
}

impl std::ops::Deref for PluginShared {
    type Target = SharedParamsInner;
    fn deref(&self) -> &SharedParamsInner { &self.inner }
}

impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

// ---------------------------------------------------------------------------
// Main thread / audio processor
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    dc_l: DcBlocker,
    dc_r: DcBlocker,
    tilt_l: Tilt,
    tilt_r: Tilt,
    // Cascaded 2× upsamplers — running them both gives 4× total. The
    // OS param selects how many stages we actually use.
    os1_l: Oversampler2x,
    os1_r: Oversampler2x,
    os2_l: Oversampler2x,
    os2_r: Oversampler2x,
    smooth_drive: SmoothedParam,
    smooth_tone: SmoothedParam,
    smooth_output: SmoothedParam,
    smooth_mix: SmoothedParam,
    sample_rate: f32,
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
}

/// Pick the curve by Type param value.
#[inline]
fn saturate(curve_idx: u32, x: f32, drive: f32) -> f32 {
    match curve_idx {
        1 => tube_clip(x, drive),
        2 => tanh_drive(x, drive),
        _ => tape_clip(x, drive),
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
        slog!("activate: sr={}", audio_config.sample_rate);
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        Ok(Self {
            shared,
            dc_l: DcBlocker::default(),
            dc_r: DcBlocker::default(),
            tilt_l: Tilt::default(),
            tilt_r: Tilt::default(),
            os1_l: Oversampler2x::default(),
            os1_r: Oversampler2x::default(),
            os2_l: Oversampler2x::default(),
            os2_r: Oversampler2x::default(),
            smooth_drive: SmoothedParam::new(load(P_DRIVE)),
            smooth_tone: SmoothedParam::new(load(P_TONE)),
            smooth_output: SmoothedParam::new(load(P_OUTPUT)),
            smooth_mix: SmoothedParam::new(load(P_MIX)),
            sample_rate: audio_config.sample_rate as f32,
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
            &self.shared.gesture_begin,
            &self.shared.gesture_end,
            events.output,
        );

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        let curve = self.shared.params[P_TYPE].load(Ordering::Relaxed).round() as u32;
        let os_mode = self.shared.params[P_OS].load(Ordering::Relaxed).round() as u32;
        let drive_db_target = self.shared.params[P_DRIVE].load(Ordering::Relaxed);
        let tone_target = self.shared.params[P_TONE].load(Ordering::Relaxed);
        let output_db_target = self.shared.params[P_OUTPUT].load(Ordering::Relaxed);
        let mix_target = self.shared.params[P_MIX].load(Ordering::Relaxed);
        let sr = self.sample_rate;

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            for (ch_idx, channel_pair) in channel_pairs.into_iter().enumerate() {
                let (dc, tilt, os1, os2) = if ch_idx == 0 {
                    (&mut self.dc_l, &mut self.tilt_l, &mut self.os1_l, &mut self.os2_l)
                } else {
                    (&mut self.dc_r, &mut self.tilt_r, &mut self.os1_r, &mut self.os2_r)
                };
                process_channel(
                    dc, tilt, os1, os2,
                    &mut self.smooth_drive, &mut self.smooth_tone,
                    &mut self.smooth_output, &mut self.smooth_mix,
                    channel_pair, sr, curve, os_mode,
                    drive_db_target, tone_target, output_db_target, mix_target,
                    bypassed,
                    &self.shared.scope,
                );
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

#[allow(clippy::too_many_arguments)]
fn process_channel(
    dc: &mut DcBlocker,
    tilt: &mut Tilt,
    os1: &mut Oversampler2x,
    os2: &mut Oversampler2x,
    smooth_drive: &mut SmoothedParam,
    smooth_tone: &mut SmoothedParam,
    smooth_output: &mut SmoothedParam,
    smooth_mix: &mut SmoothedParam,
    channel: ChannelPair<'_, f32>,
    sr: f32,
    curve: u32,
    os_mode: u32,
    drive_db_target: f32,
    tone_target: f32,
    output_db_target: f32,
    mix_target: f32,
    bypassed: bool,
    scope: &superduper_synth_core::gui::LiveScope,
) {
    use superduper_dsp_sdk::clap_helpers::split_io;
    let Some((read, write)) = split_io(channel) else { return };
    if bypassed {
        write.copy_from_slice(read);
        return;
    }

    for (i, o) in read.iter().zip(write.iter_mut()) {
        let dry = *i;

        let drive_db = smooth_drive.step(drive_db_target, sr);
        let tone = smooth_tone.step(tone_target, sr);
        let out_db = smooth_output.step(output_db_target, sr);
        let mix = smooth_mix.step(mix_target, sr);
        let drive_lin = 10f32.powf(drive_db / 20.0);
        let out_lin = 10f32.powf(out_db / 20.0);

        let cleaned = dc.process(dry);

        // Saturate at oversampled rate so the harmonics produced by the
        // non-linearity fold cleanly back into the original Nyquist.
        let saturated = match os_mode {
            0 => saturate(curve, cleaned, drive_lin),
            1 => {
                // 2×: upsample to two values, saturate each, decimate.
                let (e, odd) = os1.upsample(cleaned);
                let se = saturate(curve, e, drive_lin);
                let so = saturate(curve, odd, drive_lin);
                os1.downsample(se, so)
            }
            _ => {
                // 4×: cascade two stages.
                let (e1, o1) = os1.upsample(cleaned);
                let (e2a, o2a) = os2.upsample(e1);
                let (e2b, o2b) = os2.upsample(o1);
                let se2a = saturate(curve, e2a, drive_lin);
                let so2a = saturate(curve, o2a, drive_lin);
                let se2b = saturate(curve, e2b, drive_lin);
                let so2b = saturate(curve, o2b, drive_lin);
                let a = os2.downsample(se2a, so2a);
                let b = os2.downsample(se2b, so2b);
                os1.downsample(a, b)
            }
        };

        let toned = tilt.process(saturated, sr, tone);
        let wet = toned * out_lin;

        let final_out = dry * (1.0 - mix) + wet * mix;
        *o = final_out;
        scope.push(final_out);
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        // Sidechain on saturator is rare but enables "dynamic drive" tricks
        // (e.g. kick triggers extra grit on bass). Plumbed but currently
        // unused by the DSP — wired in a future revision.
        if is_input { 2 } else { 1 }
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
                name: b"Sidechain",
                channel_count: 2,
                flags: AudioPortFlags::empty(),
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            }),
            _ => {}
        }
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

// ---------------------------------------------------------------------------
// CLAP state — params + bypass through the shared SDK helper. Without this
// REAPER drops everything when saving the project / FX chain preset.
// ---------------------------------------------------------------------------

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
        c.api_type == GuiApiType::COCOA
            || c.api_type == GuiApiType::WIN32
            || c.api_type == GuiApiType::X11
    }
    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        let api_type = if cfg!(target_os = "macos") { GuiApiType::COCOA }
            else if cfg!(target_os = "windows") { GuiApiType::WIN32 }
            else { GuiApiType::X11 };
        Some(GuiConfiguration { api_type, is_floating: false })
    }
    fn create(&mut self, _: GuiConfiguration) -> Result<(), PluginError> { slog!("gui::create"); Ok(()) }
    fn destroy(&mut self) { slog!("gui::destroy"); self.gui_handle = None; }
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
        slog!("gui::set_parent");
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

pub struct SuperDuperSaturator;

impl Plugin for SuperDuperSaturator {
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

impl DefaultPluginFactory for SuperDuperSaturator {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.saturator",
            plugin_display_name!("SuperDuper Saturator"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Analog warmth — tape / tube / soft-clip saturation")
        .with_features([AUDIO_EFFECT, STEREO, DISTORTION])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Saturator — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperSaturator>);
