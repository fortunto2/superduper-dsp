//! SuperDuper Space — a track's place in the room.
//!
//! Stereo (or mono) in, up to 12 channels out (L R C LFE Ls Rs Lb Rb Tfl Tfr
//! Tbl Tbr): the position on a map becomes VBAP speaker gains for the chosen
//! layout, `Distance` takes level and air, `Spread` widens the source,
//! `Monitor` folds the result to the first two channels for headphones or a
//! stereo track. The `position` mailbox document lets an agent place the
//! track; the bus scope carries the stereo fold so the mix map still shows
//! the track. Not a Dolby Atmos renderer: an Apple Spatial delivery needs an
//! ADM BWF master from Dolby's tools — this is the placement in front of it.
#![allow(clippy::missing_safety_doc)]

pub mod dsp;
pub mod gui;
pub mod mail;
pub mod presets;

use atomic_float::AtomicF32;
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts, PluginAudioPortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::PluginState;
use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use clack_plugin::process::audio::ChannelPair;
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::SmoothedParam;

use dsp::{Layout, MAX_OUT};

fn init_logging() { superduper_dsp_sdk::log::init("space"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Parameters — FROZEN once shipped (hosts cache the layout).
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Layout",   min: 0.0,   max: 4.0,  default: 3.0,  unit: ""   }, // Stereo Quad 5.1 7.1 7.1.4
    ParamDef { id: 1, name: b"X",        min: -1.0,  max: 1.0,  default: 0.0,  unit: ""   }, // left … right
    ParamDef { id: 2, name: b"Y",        min: -1.0,  max: 1.0,  default: 1.0,  unit: ""   }, // back … front
    ParamDef { id: 3, name: b"Z",        min: 0.0,   max: 1.0,  default: 0.0,  unit: ""   }, // ear height … overhead (7.1.4)
    ParamDef { id: 4, name: b"Spread",   min: 0.0,   max: 1.0,  default: 0.0,  unit: ""   },
    ParamDef { id: 5, name: b"Distance", min: 0.0,   max: 1.0,  default: 0.0,  unit: ""   },
    ParamDef { id: 6, name: b"Width",    min: 0.0,   max: 2.0,  default: 1.0,  unit: ""   }, // stereo input: ±15° per unit
    ParamDef { id: 7, name: b"LFE",      min: -60.0, max: 0.0,  default: -60.0, unit: "dB" },
    ParamDef { id: 8, name: b"Output",   min: -24.0, max: 12.0, default: 0.0,  unit: "dB" },
    ParamDef { id: 9, name: b"Monitor",  min: 0.0,   max: 1.0,  default: 0.0,  unit: ""   }, // Off / Stereo fold
];
const STEPPED_PARAMS: &[u32] = &[0, 9];

pub const P_LAYOUT: usize = 0;
pub const P_X: usize = 1;
pub const P_Y: usize = 2;
pub const P_Z: usize = 3;
pub const P_SPREAD: usize = 4;
pub const P_DISTANCE: usize = 5;
pub const P_WIDTH: usize = 6;
pub const P_LFE: usize = 7;
pub const P_OUTPUT: usize = 8;
pub const P_MONITOR: usize = 9;

/// Degrees the stereo input's L and R sit either side of the position at Width 1.
const WIDTH_DEG: f32 = 15.0;

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
    /// The gains the last block used, per output channel, for the map.
    pub live_gain: [AtomicF32; MAX_OUT],
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
                scope: superduper_synth_core::gui::LiveScope::new(4096),
                active_preset: std::sync::atomic::AtomicU32::new(0),
                live_gain: std::array::from_fn(|_| AtomicF32::new(0.0)),
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
superduper_synth_core::impl_shared_host!(SharedParamsInner);
impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

/// What one block of parameters means for the panner: the per-channel gain
/// targets of the two input channels, the distance cues, the output gain.
pub struct Placement {
    pub layout: Layout,
    pub gains: [[f32; MAX_OUT]; 2],
    pub lfe: f32,
    pub level: f32,
    pub cutoff_hz: f32,
    pub monitor: bool,
}

pub fn placement(shared: &SharedParamsInner, stereo_in: bool) -> Placement {
    let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
    let layout = Layout::from_param(load(P_LAYOUT));
    let az = dsp::azimuth_deg(load(P_X), load(P_Y));
    let half = if stereo_in { load(P_WIDTH) * WIDTH_DEG } else { 0.0 };
    let (z, spread) = (load(P_Z), load(P_SPREAD));
    let (level, cutoff_hz) = dsp::distance_cues(load(P_DISTANCE));
    let out_lin = 10f32.powf(load(P_OUTPUT) / 20.0);
    let lfe_db = load(P_LFE);
    Placement {
        layout,
        gains: [dsp::gains(layout, az - half, z, spread), dsp::gains(layout, az + half, z, spread)],
        lfe: if lfe_db <= -59.9 { 0.0 } else { 10f32.powf(lfe_db / 20.0) },
        level: level * out_lin,
        cutoff_hz,
        monitor: load(P_MONITOR) >= 0.5,
    }
}

pub struct PluginMainThread<'a> {
    track_watch: superduper_synth_core::gui::TrackInfoWatch<'a>,
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    sample_rate: f32,
    /// Input copies: an in-place host hands one buffer as input and output,
    /// and every output channel needs the untouched input.
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    /// Smoothed gain per (input, output channel): a move never clicks.
    gain: [[SmoothedParam; MAX_OUT]; 2],
    lfe: SmoothedParam,
    level: SmoothedParam,
    /// One-pole air loss per input channel, follows the distance cutoff.
    air: [f32; 2],
    air_coef: f32,
    air_hz: f32,
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
}

impl<'a> PluginAudioProcessor<'a> {
    fn set_air(&mut self, hz: f32) {
        if (hz - self.air_hz).abs() > 1.0 {
            self.air_hz = hz;
            let x = (-core::f32::consts::TAU * hz.clamp(100.0, 20_000.0) / self.sample_rate).exp();
            self.air_coef = x;
        }
    }

    /// One block: the two inputs are already in `in_l` / `in_r`; write every
    /// output channel of the main port.
    fn render(&mut self, n: usize, outs: &mut [Option<&mut [f32]>; MAX_OUT], stereo_in: bool) {
        let shared = &self.shared.inner;
        let p = placement(shared, stereo_in);
        self.set_air(p.cutoff_hz);
        let sr = self.sample_rate;
        let bypassed = shared.bypass.load(Ordering::Relaxed);
        let active = p.layout.channels();
        for c in 0..MAX_OUT {
            shared.live_gain[c].store(if bypassed { 0.0 } else { (p.gains[0][c] + p.gains[1][c]) * 0.5 }, Ordering::Relaxed);
        }
        if bypassed {
            // The input on its own two channels, the rest silent: what the
            // host hears with the plugin off, latency none.
            for (c, o) in outs.iter_mut().enumerate() {
                if let Some(o) = o {
                    match c {
                        0 => o[..n].copy_from_slice(&self.in_l[..n]),
                        1 => o[..n].copy_from_slice(&self.in_r[..n]),
                        _ => o[..n].fill(0.0),
                    }
                }
            }
            for i in 0..n { shared.scope.push(0.5 * (self.in_l[i] + self.in_r[i])); }
            return;
        }
        let fold = dsp::fold_weights(p.layout);
        let mut mix = [0.0f32; MAX_OUT];
        for i in 0..n {
            let lvl = self.level.step(p.level, sr);
            let lfe = self.lfe.step(p.lfe, sr);
            // air: one pole, state per input channel
            self.air[0] += (1.0 - self.air_coef) * (self.in_l[i] - self.air[0]);
            self.air[1] += (1.0 - self.air_coef) * (self.in_r[i] - self.air[1]);
            let (xl, xr) = (self.air[0] * lvl, self.air[1] * lvl);
            mix.fill(0.0);
            for c in 0..active {
                let gl = self.gain[0][c].step(p.gains[0][c], sr);
                let gr = self.gain[1][c].step(p.gains[1][c], sr);
                mix[c] = xl * gl + xr * gr;
            }
            if let Some(c) = p.layout.lfe() {
                mix[c] = 0.5 * (xl + xr) * lfe;
            }
            let (fl, fr) = mix.iter().enumerate().take(active).fold((0.0, 0.0), |acc, (c, v)| (acc.0 + v * fold[c].0, acc.1 + v * fold[c].1));
            shared.scope.push(0.5 * (fl + fr));
            if p.monitor {
                mix.fill(0.0);
                mix[0] = fl;
                mix[1] = fr;
            }
            for (c, o) in outs.iter_mut().enumerate() {
                if let Some(o) = o { o[i] = mix[c]; }
            }
        }
    }
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
        shared.scope.set_sample_rate(cfg.sample_rate as f32);
        let sr = cfg.sample_rate as f32;
        slog!("activate sr={} max_frames={}", sr, cfg.max_frames_count);
        let cap = cfg.max_frames_count as usize;
        let p = placement(shared, true);
        Ok(Self {
            shared,
            sample_rate: sr,
            in_l: vec![0.0; cap],
            in_r: vec![0.0; cap],
            gain: std::array::from_fn(|s| std::array::from_fn(|c| SmoothedParam::new(p.gains[s][c]))),
            lfe: SmoothedParam::new(p.lfe),
            level: SmoothedParam::new(p.level),
            air: [0.0; 2],
            air_coef: 0.0,
            air_hz: 0.0,
        })
    }

    fn process(&mut self, _process: Process, mut audio: Audio, mut events: Events) -> Result<ProcessStatus, PluginError> {
        let _denormals = superduper_dsp_sdk::denormals::Guard::new();
        let shared = self.shared;
        superduper_synth_core::effect::begin_events(&**shared, &mut events);

        // The main port pairs a 2-channel input with a 12-channel output:
        // channels 0–1 come as input+output pairs (or one in-place buffer),
        // 2–11 as output-only. Copy the input first — an in-place buffer is
        // overwritten by the first output written.
        let Some(mut port) = audio.port_pair(0) else { return Ok(ProcessStatus::Continue) };
        let Some(pairs) = port.channels()?.into_f32() else { return Ok(ProcessStatus::Continue) };
        let mut outs: [Option<&mut [f32]>; MAX_OUT] = Default::default();
        let mut n = 0usize;
        let mut stereo_in = false;
        for (c, pair) in pairs.into_iter().enumerate() {
            let (input, output): (Option<&[f32]>, Option<&mut [f32]>) = match pair {
                ChannelPair::InputOutput(i, o) => (Some(i), Some(o)),
                ChannelPair::InPlace(b) => (Some(unsafe { &*(b as *const [f32]) }), Some(b)),
                ChannelPair::OutputOnly(o) => (None, Some(o)),
                ChannelPair::InputOnly(i) => (Some(i), None),
            };
            if let Some(i) = input {
                let k = i.len().min(self.in_l.len());
                n = n.max(k);
                match c {
                    0 => self.in_l[..k].copy_from_slice(&i[..k]),
                    1 => { self.in_r[..k].copy_from_slice(&i[..k]); stereo_in = true; }
                    _ => {}
                }
            }
            if c < MAX_OUT {
                if let Some(o) = output { n = n.max(o.len().min(self.in_l.len())); outs[c] = Some(o); }
            }
        }
        if !stereo_in {
            let k = n;
            self.in_r[..k].copy_from_slice(&self.in_l[..k]);
        }
        for o in outs.iter_mut().flatten() {
            let k = o.len().min(n);
            o[k..].fill(0.0);
        }
        let n = outs.iter().flatten().map(|o| o.len()).min().unwrap_or(0).min(n);
        self.render(n, &mut outs, stereo_in);
        Ok(ProcessStatus::Continue)
    }
}

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 { 1 }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index != 0 { return; }
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: if is_input { b"Input" } else { b"Speakers" },
            channel_count: if is_input { 2 } else { MAX_OUT as u32 },
            flags: AudioPortFlags::IS_MAIN,
            port_type: if is_input { Some(AudioPortType::STEREO) } else { None },
            in_place_pair: None,
        });
    }
}

impl PluginMainThread<'_> {
    /// What a parameter value reads as — the one place for it, so the host's
    /// text entry (`text_to_value`) accepts exactly what the display shows.
    fn display_text<W: core::fmt::Write>(&mut self, id: ClapId, v: f64, w: &mut W) -> core::fmt::Result {
        match id.get() as usize {
            P_LAYOUT => superduper_dsp_sdk::clap_helpers::enum_value_to_text(&dsp::LAYOUT_NAMES, v, w),
            P_MONITOR => superduper_dsp_sdk::clap_helpers::enum_value_to_text(&["Off", "Stereo fold"], v, w),
            P_LFE if v <= -59.9 => write!(w, "off"),
            _ => ParamDef::write_display(PARAMS, id, v, w),
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
        self.display_text(id, v, w)
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        superduper_dsp_sdk::clap_helpers::ParamDef::text_to_value_by_display(PARAMS, id, t, |v, s| self.display_text(id, v, s))
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

use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl, Window as ClapGuiWindow,
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
        Some(GuiResizeHints { can_resize_horizontally: true, can_resize_vertically: true, strategy: AspectRatioStrategy::Disregard })
    }
    fn adjust_size(&mut self, s: GuiSize) -> Option<GuiSize> {
        Some(GuiSize { width: s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH), height: s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT) })
    }
    fn set_size(&mut self, s: GuiSize) -> Result<(), PluginError> {
        self.gui_resize.0.store(s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH), AtomicOrdering::Relaxed);
        self.gui_resize.1.store(s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT), AtomicOrdering::Relaxed);
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

pub struct SuperDuperSpace;

impl Plugin for SuperDuperSpace {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::track_info::PluginTrackInfo>();
    }
}

impl DefaultPluginFactory for SuperDuperSpace {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("co.superduperai.space", plugin_display_name!("SuperDuper Space"))
            .with_vendor("SuperDuperAI")
            .with_version(version_string!("0.1"))
            .with_description("Spatial panner: VBAP placement to stereo, quad, 5.1, 7.1 or 7.1.4 with distance cues and an agent-reachable position")
            .with_features([AUDIO_EFFECT, SURROUND, UTILITY])
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Space — build {} ({})", build_num!(), build_date!());
        let shared = PluginShared::new();
        superduper_synth_core::gui::spawn_bus_publisher_mcp(
            "SuperDuper Space",
            &shared.inner,
            |s| &s.scope,
            Some(superduper_synth_core::gui::ParamsAccess { defs: PARAMS, values: |s| &s.params, dirty: Some(|s| &s.dirty_params) }),
            Some(mail::handle),
        );
        Ok(shared)
    }
    fn new_main_thread<'a>(host: HostMainThreadHandle<'a>, shared: &'a PluginShared) -> Result<PluginMainThread<'a>, PluginError> {
        let track_watch = superduper_synth_core::gui::TrackInfoWatch::new(host, &shared.scope);
        Ok(PluginMainThread { track_watch, shared, gui_handle: None, gui_resize: gui::new_resize_bridge() })
    }
}

clack_export_entry!(SinglePluginEntry<SuperDuperSpace>);

impl clack_extensions::track_info::PluginTrackInfoImpl for PluginMainThread<'_> {
    fn changed(&self) {
        self.track_watch.refresh();
    }
}
