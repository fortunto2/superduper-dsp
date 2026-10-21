//! SuperDuper Multiband — 2–4 band compressor with dynamic phase and a
//! per-band key from the SuperDuper bus.
//!
//! The DSP lives in [`dsp`] (pure, tested without a host); this file is the
//! CLAP glue: parameter table, shared state, meters, extensions.
//!
//! The bus key is the part no other multiband has: pick a track on the bus
//! (one per instance) and each band can duck from *that track's level in the
//! band's own frequency range* — the bass band ducks when the kick's low end
//! hits, the 2–5 kHz band ducks under the vocal's presence, while the other
//! bands keep compressing on their own signal. `B<n> Key` blends: 0 = own
//! signal, 100 % = the key alone.

#![allow(clippy::missing_safety_doc)]

pub mod dsp;
pub mod gui;
mod gui_spectrum;
pub mod mail; // per-band gain-reduction meter as a read-only bus document
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
use dsp::{BandSettings, Engine, Settings, MAX_BANDS, MAX_XOVERS};
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};

fn init_logging() { superduper_dsp_sdk::log::init("multiband"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Params: 6 globals, then 10 per band × 4 bands
// ---------------------------------------------------------------------------

pub const P_BANDS: usize = 0;
pub const P_XOVER: [usize; MAX_XOVERS] = [1, 2, 3];
pub const P_MIX: usize = 4;
pub const P_OUTPUT: usize = 5;
const BAND_BASE: usize = 6;
pub const PER_BAND: usize = 10;

pub const F_THRESHOLD: usize = 0;
pub const F_RATIO: usize = 1;
pub const F_ATTACK: usize = 2;
pub const F_RELEASE: usize = 3;
pub const F_KNEE: usize = 4;
pub const F_MAKEUP: usize = 5;
pub const F_RANGE: usize = 6;
pub const F_KEY: usize = 7;
pub const F_SOLO: usize = 8;
pub const F_BYPASS: usize = 9;

/// Index of band `band`'s field `field` (F_*) in PARAMS.
pub const fn bp(band: usize, field: usize) -> usize { BAND_BASE + band * PER_BAND + field }

macro_rules! band_params {
    ($n:literal, $base:expr) => {[
        ParamDef { id: $base,     name: concat!("B", $n, " Threshold").as_bytes(), min: -60.0, max: 0.0,    default: -20.0, unit: "dB" },
        // Below 1:1 the band gets louder above threshold (upward expansion),
        // capped at +12 dB unless Range sets the cap.
        ParamDef { id: $base + 1, name: concat!("B", $n, " Ratio").as_bytes(),     min: 0.5,   max: 20.0,   default: 2.0,   unit: ":1" },
        ParamDef { id: $base + 2, name: concat!("B", $n, " Attack").as_bytes(),    min: 0.1,   max: 200.0,  default: 10.0,  unit: "ms" },
        ParamDef { id: $base + 3, name: concat!("B", $n, " Release").as_bytes(),   min: 5.0,   max: 2000.0, default: 120.0, unit: "ms" },
        ParamDef { id: $base + 4, name: concat!("B", $n, " Knee").as_bytes(),      min: 0.0,   max: 24.0,   default: 6.0,   unit: "dB" },
        ParamDef { id: $base + 5, name: concat!("B", $n, " Makeup").as_bytes(),    min: -24.0, max: 24.0,   default: 0.0,   unit: "dB" },
        // Most the band's gain may move, either way. 0 = unlimited.
        ParamDef { id: $base + 6, name: concat!("B", $n, " Range").as_bytes(),     min: 0.0,   max: 48.0,   default: 0.0,   unit: "dB" },
        // How much the bus key track (its level in this band's range)
        // drives the detector instead of the band's own signal.
        ParamDef { id: $base + 7, name: concat!("B", $n, " Key").as_bytes(),       min: 0.0,   max: 100.0,  default: 0.0,   unit: "%"  },
        ParamDef { id: $base + 8, name: concat!("B", $n, " Solo").as_bytes(),      min: 0.0,   max: 1.0,    default: 0.0,   unit: ""   },
        ParamDef { id: $base + 9, name: concat!("B", $n, " Bypass").as_bytes(),    min: 0.0,   max: 1.0,    default: 0.0,   unit: ""   },
    ]};
}

const GLOBALS: [ParamDef; BAND_BASE] = [
    ParamDef { id: 0, name: b"Bands",   min: 2.0,    max: 4.0,      default: 3.0,    unit: ""   },
    ParamDef { id: 1, name: b"Xover 1", min: 20.0,   max: 20000.0,  default: 150.0,  unit: "Hz" },
    ParamDef { id: 2, name: b"Xover 2", min: 20.0,   max: 20000.0,  default: 2500.0, unit: "Hz" },
    ParamDef { id: 3, name: b"Xover 3", min: 20.0,   max: 20000.0,  default: 8000.0, unit: "Hz" },
    ParamDef { id: 4, name: b"Mix",     min: 0.0,    max: 1.0,      default: 1.0,    unit: ""   },
    ParamDef { id: 5, name: b"Output",  min: -24.0,  max: 24.0,     default: 0.0,    unit: "dB" },
];
const B1: [ParamDef; PER_BAND] = band_params!(1, 6);
const B2: [ParamDef; PER_BAND] = band_params!(2, 16);
const B3: [ParamDef; PER_BAND] = band_params!(3, 26);
const B4: [ParamDef; PER_BAND] = band_params!(4, 36);
pub const N_PARAMS: usize = BAND_BASE + MAX_BANDS * PER_BAND;

const fn concat_params() -> [ParamDef; N_PARAMS] {
    let parts: [&[ParamDef]; 5] = [&GLOBALS, &B1, &B2, &B3, &B4];
    let mut out = [GLOBALS[0]; N_PARAMS];
    let (mut p, mut k) = (0, 0);
    while p < parts.len() {
        let mut i = 0;
        while i < parts[p].len() {
            out[k] = parts[p][i];
            k += 1;
            i += 1;
        }
        p += 1;
    }
    out
}

const PARAM_TABLE: [ParamDef; N_PARAMS] = concat_params();
pub const PARAMS: &[ParamDef] = &PARAM_TABLE;

/// Discrete params: band count, and every band's Solo / Bypass.
pub const STEPPED_PARAMS: &[u32] = &[0, 14, 15, 24, 25, 34, 35, 44, 45];

/// Read every param once into the engine's settings (per block).
pub fn settings_from(params: &[AtomicF32]) -> Settings {
    let v = |i: usize| params[i].load(Ordering::Relaxed);
    Settings {
        bands: v(P_BANDS).round().clamp(2.0, MAX_BANDS as f32) as usize,
        xover_hz: std::array::from_fn(|i| v(P_XOVER[i])),
        mix: v(P_MIX),
        output_db: v(P_OUTPUT),
        band: std::array::from_fn(|b| BandSettings {
            threshold_db: v(bp(b, F_THRESHOLD)),
            ratio: v(bp(b, F_RATIO)),
            attack_ms: v(bp(b, F_ATTACK)),
            release_ms: v(bp(b, F_RELEASE)),
            knee_db: v(bp(b, F_KNEE)),
            makeup_db: v(bp(b, F_MAKEUP)),
            range_db: v(bp(b, F_RANGE)),
            key: v(bp(b, F_KEY)) * 0.01,
            solo: v(bp(b, F_SOLO)) >= 0.5,
            bypass: v(bp(b, F_BYPASS)) >= 0.5,
        }),
    }
}

// ---------------------------------------------------------------------------
// Shared state (audio thread writes meters, GUI / bus read them)
// ---------------------------------------------------------------------------

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

/// Per-band meters. `gr_db` has a 150 ms fall so the overlay reads; `max_gr_db`
/// holds the most extreme value for ~0.7 s (what an agent asks for).
pub struct BandMeters {
    pub gr_db: [AtomicF32; MAX_BANDS],
    pub max_gr_db: [AtomicF32; MAX_BANDS],
    pub detector_db: [AtomicF32; MAX_BANDS],
    pub key_db: [AtomicF32; MAX_BANDS],
}

impl BandMeters {
    fn new() -> Self {
        let z = |v: f32| std::array::from_fn(|_| AtomicF32::new(v));
        Self { gr_db: z(0.0), max_gr_db: z(0.0), detector_db: z(-120.0), key_db: z(-120.0) }
    }
}

pub struct SharedParamsInner {
    pub params: [AtomicF32; N_PARAMS],
    pub bypass: AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub dirty_params: [AtomicBool; N_PARAMS],
    pub gesture_begin: [AtomicBool; N_PARAMS],
    pub gesture_end: [AtomicBool; N_PARAMS],
    pub active_preset: AtomicU32,
    pub meters: BandMeters,
    /// Output, mono: the spectrum behind the bands and the SuperDuper bus.
    pub scope: superduper_synth_core::gui::LiveScope,
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
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(N_PARAMS),
                dirty_params: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| AtomicBool::new(false)),
                active_preset: AtomicU32::new(0),
                meters: BandMeters::new(),
                scope: superduper_synth_core::gui::LiveScope::new(4096),
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

const METER_HOLD_S: f32 = 0.7;

pub struct PluginMainThread<'a> {
    track_watch: superduper_synth_core::gui::TrackInfoWatch<'a>,
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    engine: Engine,
    sample_rate: f32,
    /// Mono: R is a copy of L through the same stereo engine.
    mono_r_in: Box<[f32]>,
    mono_r_out: Box<[f32]>,
    meter_fall: [f32; MAX_BANDS],
    hold: [f32; MAX_BANDS],
    hold_left: [f32; MAX_BANDS],
}

impl PluginAudioProcessor<'_> {
    fn publish_meters(&mut self, rep: &dsp::BlockReport, key_db: &[f32; MAX_BANDS], frames: usize) {
        let m = &self.shared.meters;
        let fall = (-(frames as f32) / (0.15 * self.sample_rate)).exp();
        for b in 0..MAX_BANDS {
            let g = rep.gr_db[b];
            self.meter_fall[b] = if g.abs() >= self.meter_fall[b].abs() { g } else { g + (self.meter_fall[b] - g) * fall };
            self.hold_left[b] -= frames as f32;
            if g.abs() >= self.hold[b].abs() || self.hold_left[b] <= 0.0 {
                self.hold[b] = g;
                self.hold_left[b] = METER_HOLD_S * self.sample_rate;
            }
            m.gr_db[b].store(self.meter_fall[b], Ordering::Relaxed);
            m.max_gr_db[b].store(self.hold[b], Ordering::Relaxed);
            m.detector_db[b].store(rep.detector_db[b], Ordering::Relaxed);
            m.key_db[b].store(key_db[b], Ordering::Relaxed);
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
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        shared.scope.set_sample_rate(sr);
        slog!("activate sr={}", sr);
        let max_frames = audio_config.max_frames_count as usize;
        Ok(Self {
            shared,
            engine: Engine::new(sr, &settings_from(&shared.params)),
            sample_rate: sr,
            mono_r_in: vec![0.0; max_frames].into_boxed_slice(),
            mono_r_out: vec![0.0; max_frames].into_boxed_slice(),
            meter_fall: [0.0; MAX_BANDS],
            hold: [0.0; MAX_BANDS],
            hold_left: [0.0; MAX_BANDS],
        })
    }

    fn process(&mut self, _process: Process, mut audio: Audio, events: Events) -> Result<ProcessStatus, PluginError> {
        let _denormals = superduper_dsp_sdk::denormals::Guard::new();
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, events.input);
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(&self.shared.params, &self.shared.dirty_params, events.output);
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(&self.shared.gesture_begin, &self.shared.gesture_end, events.output);

        let settings = settings_from(&self.shared.params);
        // Bus key, once per block: atomics only (the publisher thread fills them).
        let key_db = dsp::key_levels(&self.shared.scope, &settings, self.sample_rate);
        let bypassed = self.shared.bypass.load(Ordering::Relaxed);

        let Some(mut main_pair) = audio.port_pair(0) else { return Ok(ProcessStatus::Continue) };
        let Some(channel_pairs) = main_pair.channels()?.into_f32() else { return Ok(ProcessStatus::Continue) };
        let mut iter = channel_pairs.into_iter();
        let Some(ch_l) = iter.next() else { return Ok(ProcessStatus::Continue) };
        use superduper_dsp_sdk::clap_helpers::split_io;
        let Some((l_read, l_write)) = split_io(ch_l) else { return Ok(ProcessStatus::Continue) };
        let r = iter.next().and_then(split_io);

        if bypassed {
            l_write.copy_from_slice(l_read);
            if let Some((rr, rw)) = r { rw.copy_from_slice(rr); }
            self.publish_meters(&dsp::BlockReport::default(), &key_db, l_read.len());
            return Ok(ProcessStatus::Continue);
        }

        let n = l_read.len();
        let rep = if let Some((r_read, r_write)) = r {
            let rep = self.engine.process(&settings, &key_db, l_read, r_read, l_write, r_write);
            for i in 0..n.min(r_write.len()) { self.shared.scope.push(0.5 * (l_write[i] + r_write[i])); }
            rep
        } else {
            let n = n.min(self.mono_r_in.len());
            self.mono_r_in[..n].copy_from_slice(&l_read[..n]);
            let rep = self.engine.process(&settings, &key_db, &l_read[..n], &self.mono_r_in[..n], &mut l_write[..n], &mut self.mono_r_out[..n]);
            for x in &l_write[..n] { self.shared.scope.push(*x); }
            rep
        };
        self.publish_meters(&rep, &key_db, n);
        Ok(ProcessStatus::Continue)
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 { 1 }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: if is_input { b"Input" } else { b"Output" },
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                // Separate in/out buffers: split_io must not alias (see compressor).
                in_place_pair: None,
            });
        }
    }
}

impl PluginMainThread<'_> {
    /// What a parameter value reads as — the one place for it, so the host's
    /// text entry (`text_to_value`) accepts exactly what the display shows.
    fn display_text<W: core::fmt::Write>(&mut self, id: ClapId, v: f64, w: &mut W) -> core::fmt::Result {
        use core::fmt::Write;
        let pid = id.get() as usize;
        if pid >= BAND_BASE && matches!((pid - BAND_BASE) % PER_BAND, F_SOLO | F_BYPASS) {
            return write!(w, "{}", if v >= 0.5 { "On" } else { "Off" });
        }
        if pid == P_MIX {
            return write!(w, "{:.0} %", v * 100.0);
        }
        ParamDef::write_display(PARAMS, id, v, w)
    }
}

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 { N_PARAMS as u32 }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info_stepped(PARAMS, idx, info, STEPPED_PARAMS);
    }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        self.shared.params.get(id.get() as usize).map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(&mut self, id: ClapId, v: f64, w: &mut ParamDisplayWriter) -> core::fmt::Result {
        self.display_text(id, v, w)
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> { superduper_dsp_sdk::clap_helpers::ParamDef::text_to_value_by_display(PARAMS, id, t, |v, s| self.display_text(id, v, s)) }
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
    }
}

// Params + bypass + preset + the bus key track.
superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);

use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window as ClapGuiWindow,
};

impl PluginGuiImpl for PluginMainThread<'_> {
    fn is_api_supported(&mut self, c: GuiConfiguration) -> bool {
        !c.is_floating && (c.api_type == GuiApiType::COCOA || c.api_type == GuiApiType::WIN32 || c.api_type == GuiApiType::X11)
    }
    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        let api_type = if cfg!(target_os = "macos") { GuiApiType::COCOA }
            else if cfg!(target_os = "windows") { GuiApiType::WIN32 }
            else { GuiApiType::X11 };
        Some(GuiConfiguration { api_type, is_floating: false })
    }
    fn create(&mut self, _: GuiConfiguration) -> Result<(), PluginError> { Ok(()) }
    fn destroy(&mut self) { self.gui_handle = None; }
    fn set_scale(&mut self, _: f64) -> Result<(), PluginError> { Ok(()) }
    fn get_size(&mut self) -> Option<GuiSize> {
        let (width, height) = superduper_synth_core::gui::read_bridge(&self.gui_resize);
        Some(GuiSize { width, height })
    }
    fn can_resize(&mut self) -> bool { true }
    fn get_resize_hints(&mut self) -> Option<GuiResizeHints> {
        Some(GuiResizeHints { can_resize_horizontally: true, can_resize_vertically: true, strategy: AspectRatioStrategy::Disregard })
    }
    fn adjust_size(&mut self, s: GuiSize) -> Option<GuiSize> {
        Some(GuiSize { width: s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH), height: s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT) })
    }
    fn set_size(&mut self, s: GuiSize) -> Result<(), PluginError> {
        superduper_synth_core::gui::write_bridge(
            &self.gui_resize,
            s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH),
            s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT),
        );
        Ok(())
    }
    fn set_parent(&mut self, window: ClapGuiWindow) -> Result<(), PluginError> {
        self.gui_handle = Some(gui::open_window(&window, self.shared.shared_handle(), self.gui_resize.clone()));
        Ok(())
    }
    fn set_transient(&mut self, _: ClapGuiWindow) -> Result<(), PluginError> { Ok(()) }
    fn show(&mut self) -> Result<(), PluginError> { Ok(()) }
    fn hide(&mut self) -> Result<(), PluginError> { Ok(()) }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct SuperDuperMultiband;

impl Plugin for SuperDuperMultiband {
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

impl DefaultPluginFactory for SuperDuperMultiband {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("co.superduperai.multiband", plugin_display_name!("SuperDuper Multiband"))
            .with_vendor("SuperDuperAI")
            .with_version(version_string!("0.1"))
            .with_description("2-4 band compressor, dynamic phase, per-band key from the SuperDuper bus")
            .with_features([AUDIO_EFFECT, STEREO, COMPRESSOR])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Multiband — build {} ({})", build_num!(), build_date!());
        let shared = PluginShared::new();
        superduper_synth_core::gui::spawn_bus_publisher_mcp(
            "SuperDuper Multiband",
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

clack_export_entry!(SinglePluginEntry<SuperDuperMultiband>);

impl clack_extensions::track_info::PluginTrackInfoImpl for PluginMainThread<'_> {
    fn changed(&self) { self.track_watch.refresh(); }
}
