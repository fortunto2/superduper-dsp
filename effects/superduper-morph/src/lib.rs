//! SuperDuper Morph — real-time timbre morph. The main input (a voice, any
//! monophonic line) keeps its performance — pitch, timing, dynamics,
//! consonants — while its timbre moves toward a target, continuously with
//! `Morph` 0 → 1. Spectral modelling, no neural net.
//!
//! Targets (`Source`): **Sidechain** — a second audio input, analysed live the
//! same way as the main one; **Captured** — a frozen snapshot of the sidechain
//! (Capture button / mailbox, saved with the project); **Model** — the
//! parametric Kubyz (factory `Target` presets or the mailbox `target` doc).
//!
//! Modes: **Harmonic** moves each harmonic of the voice toward the target's
//! harmonic of the same number, in dB; **Envelope** moves the voice's smoothed
//! spectral envelope toward the target's and works on anything, pitched or
//! not. Harmonic falls back to Envelope on frames where either side is
//! unvoiced. DSP: `engine.rs` + `superduper_synth_core::timbre`.

#![allow(clippy::missing_safety_doc)]

pub mod engine;
pub mod gui;
pub mod mail;
pub mod presets;
pub mod targets;

use atomic_float::AtomicF32;
use clack_common::stream::{InputStream, OutputStream};
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams,
    PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use superduper_dsp_sdk::clap_helpers::{ParamDef, SidechainSnapshot};
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, slog, version_string};
use superduper_synth_core::timbre::{FrameMode, FLOOR_DB, MAX_HARM};

use engine::{MorphEngine, Settings, Source};
use targets::{ModelStore, ProfileStore, KUBYZ_COUNT};

fn init_logging() {
    superduper_dsp_sdk::log::init("morph");
}

// ---------------------------------------------------------------------------
// Params — layout FROZEN once shipped.
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    // 0 = the voice untouched (exactly: the delayed dry), 1 = the target's timbre.
    ParamDef { id: 0, name: b"Morph",  min: 0.0,   max: 1.0,  default: 0.8, unit: "" },
    // 0 = the target's mouth/envelope, 1 = the voice's own (the target "speaks" the words).
    ParamDef { id: 1, name: b"Vowels", min: 0.0,   max: 1.0,  default: 0.0, unit: "" },
    // Breath / noise between the harmonics at Morph 1 (0 = gone, 1 = as sung).
    ParamDef { id: 2, name: b"Breath", min: 0.0,   max: 1.0,  default: 0.3, unit: "" },
    // 0 Sidechain, 1 Captured, 2 Model.
    ParamDef { id: 3, name: b"Source", min: 0.0,   max: 2.0,  default: 2.0, unit: "" },
    // 0 Harmonic, 1 Envelope.
    ParamDef { id: 4, name: b"Mode",   min: 0.0,   max: 1.0,  default: 0.0, unit: "" },
    // Kubyz factory preset for Source = Model.
    ParamDef { id: 5, name: b"Target", min: 0.0,   max: (KUBYZ_COUNT - 1) as f64, default: 1.0, unit: "" },
    // Extra brightness at Morph 1, dB per octave around 1 kHz.
    ParamDef { id: 6, name: b"Tilt",   min: -6.0,  max: 6.0,  default: 0.0, unit: "dB/oct" },
    ParamDef { id: 7, name: b"Mix",    min: 0.0,   max: 1.0,  default: 1.0, unit: "" },
    ParamDef { id: 8, name: b"Output", min: -24.0, max: 24.0, default: 0.0, unit: "dB" },
];
pub const STEPPED_PARAMS: &[u32] = &[3, 4, 5];

pub const P_MORPH: usize = 0;
pub const P_VOWELS: usize = 1;
pub const P_BREATH: usize = 2;
pub const P_SOURCE: usize = 3;
pub const P_MODE: usize = 4;
pub const P_TARGET: usize = 5;
pub const P_TILT: usize = 6;
pub const P_MIX: usize = 7;
pub const P_OUTPUT: usize = 8;

pub const SOURCE_NAMES: [&str; 3] = ["Sidechain", "Captured", "Model"];
pub const MODE_NAMES: [&str; 2] = ["Harmonic", "Envelope"];

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// What the audio thread last saw, for the GUI and the mailbox.
pub struct Live {
    pub f0: AtomicF32,
    pub voiced: AtomicBool,
    /// 0 pass, 1 harmonic, 2 envelope.
    pub mode: AtomicU32,
    pub sc_f0: AtomicF32,
    pub sc_voiced: AtomicBool,
    pub sc_level_db: AtomicF32,
    pub n: AtomicU32,
    pub voice: [AtomicF32; MAX_HARM],
    pub target: [AtomicF32; MAX_HARM],
    pub out: [AtomicF32; MAX_HARM],
}

impl Live {
    fn new() -> Self {
        let a = || std::array::from_fn(|_| AtomicF32::new(FLOOR_DB));
        Self {
            f0: AtomicF32::new(0.0),
            voiced: AtomicBool::new(false),
            mode: AtomicU32::new(0),
            sc_f0: AtomicF32::new(0.0),
            sc_voiced: AtomicBool::new(false),
            sc_level_db: AtomicF32::new(FLOOR_DB),
            n: AtomicU32::new(0),
            voice: a(),
            target: a(),
            out: a(),
        }
    }
}

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub dirty_params: [AtomicBool; PARAMS.len()],
    pub gesture_begin: [AtomicBool; PARAMS.len()],
    pub gesture_end: [AtomicBool; PARAMS.len()],
    pub active_preset: AtomicU32,
    pub latency_samples: AtomicU32,
    pub model: ModelStore,
    pub captured: ProfileStore,
    /// Set by the GUI / mailbox, consumed by the audio thread.
    pub capture_request: AtomicBool,
    pub live: Live,
}

impl SharedParamsInner {
    /// Set a param from outside the host (GUI / mailbox) so the host sees it.
    pub fn write_param(&self, i: usize, v: f32) {
        if let (Some(a), Some(d)) = (self.params.get(i), self.dirty_params.get(i)) {
            a.store(v, Ordering::Relaxed);
            d.store(true, Ordering::Relaxed);
        }
    }

    pub fn save_state_json(&self) -> String {
        let captured = self.captured.load();
        let st = targets::MorphState {
            version: targets::STATE_VERSION,
            params: self.params.iter().map(|a| a.load(Ordering::Relaxed)).collect(),
            bypass: self.bypass.load(Ordering::Relaxed),
            active_preset: self.active_preset.load(Ordering::Relaxed),
            model: targets::ModelJson::from_model(&self.model.load()),
            captured: captured.env_valid.then(|| targets::ProfileJson::from_profile(&captured)),
        };
        serde_json::to_string(&st).unwrap_or_default()
    }

    pub fn load_state_json(&self, json: &str) -> Result<(), String> {
        let st: targets::MorphState = serde_json::from_str(json).map_err(|e| e.to_string())?;
        for (i, v) in st.params.iter().enumerate().take(PARAMS.len()) {
            let d = &PARAMS[i];
            self.params[i].store(v.clamp(d.min as f32, d.max as f32), Ordering::Relaxed);
        }
        self.bypass.store(st.bypass, Ordering::Relaxed);
        self.active_preset.store(st.active_preset, Ordering::Relaxed);
        let target = self.params[P_TARGET].load(Ordering::Relaxed).round() as u32;
        self.model.from_target.store(target, Ordering::Relaxed);
        self.model.store(&st.model.to_model()?);
        let cap = match &st.captured {
            Some(p) => p.to_profile()?,
            None => Default::default(),
        };
        self.captured.store(&cap);
        Ok(())
    }
}

pub struct PluginShared {
    pub inner: SharedParams,
}

impl PluginShared {
    pub fn new() -> Self {
        let target = PARAMS[P_TARGET].default as usize;
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| AtomicBool::new(false)),
                active_preset: AtomicU32::new(0),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(4096),
                latency_samples: AtomicU32::new(engine::LATENCY as u32),
                model: ModelStore::new(&targets::kubyz_model(target), target as u32),
                captured: ProfileStore::new(),
                capture_request: AtomicBool::new(false),
                live: Live::new(),
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
    track_watch: superduper_synth_core::gui::TrackInfoWatch<'a>,
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    engine: Box<MorphEngine>,
    sc: SidechainSnapshot,
    model_gen: u32,
    captured_gen: u32,
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
        shared.scope.set_sample_rate(sr);
        slog!("activate sr={} max_frames={}", sr, cfg.max_frames_count);
        let mut engine = Box::new(MorphEngine::new(sr, shared.model.load()));
        engine.captured = shared.captured.load();
        Ok(Self {
            shared,
            engine,
            sc: SidechainSnapshot::new(cfg.max_frames_count as usize),
            model_gen: shared.model.gen.load(Ordering::Acquire),
            captured_gen: shared.captured.gen.load(Ordering::Acquire),
        })
    }

    fn process(&mut self, _p: Process, mut audio: Audio, events: Events) -> Result<ProcessStatus, PluginError> {
        let _denormals = superduper_dsp_sdk::denormals::Guard::new();
        let sh = self.shared;
        apply_param_events(sh, events.input);
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(&sh.params, &sh.dirty_params, events.output);
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(&sh.gesture_begin, &sh.gesture_end, events.output);
        self.sync_targets();
        let settings = self.settings();
        let bypassed = sh.bypass.load(Ordering::Relaxed);
        self.sc.capture(&mut audio, 1)?;

        if let Some(mut main_pair) = audio.port_pair(0) {
            let Some(chans) = main_pair.channels()?.into_f32() else {
                return Ok(ProcessStatus::Continue);
            };
            let mut it = chans.into_iter();
            let Some(ch_l) = it.next() else { return Ok(ProcessStatus::Continue) };
            use superduper_dsp_sdk::clap_helpers::split_io;
            let Some((rl, wl)) = split_io(ch_l) else { return Ok(ProcessStatus::Continue) };
            let mut r = it.next().and_then(split_io);
            if bypassed {
                wl.copy_from_slice(rl);
                if let Some((rr, wr)) = r {
                    wr.copy_from_slice(rr);
                }
                return Ok(ProcessStatus::Continue);
            }
            let n = rl.len().min(wl.len()).min(self.sc.l.len());
            for i in 0..n {
                let xl = rl[i];
                let xr = r.as_ref().and_then(|(rr, _)| rr.get(i).copied()).unwrap_or(xl);
                let (sl, sr) = (self.sc.l[i], self.sc.r[i]);
                let (ol, or) = self.engine.process(xl, xr, sl, sr, &settings);
                wl[i] = ol;
                if let Some((_, wr)) = r.as_mut() {
                    if let Some(w) = wr.get_mut(i) {
                        *w = or;
                    }
                }
                sh.scope.push(0.5 * (ol + or));
            }
        }
        self.publish_live();
        Ok(ProcessStatus::Continue)
    }
}

impl PluginAudioProcessor<'_> {
    fn settings(&self) -> Settings {
        let p = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        Settings {
            morph: p(P_MORPH),
            vowels: p(P_VOWELS),
            breath: p(P_BREATH),
            tilt_db_oct: p(P_TILT),
            mix: p(P_MIX),
            output_db: p(P_OUTPUT),
            source: Source::from_param(p(P_SOURCE)),
            harmonic: p(P_MODE) < 0.5,
        }
    }

    /// Target param → factory model; generation-checked store reads; Capture.
    fn sync_targets(&mut self) {
        let sh = self.shared;
        let t = sh.params[P_TARGET].load(Ordering::Relaxed).round().clamp(0.0, (KUBYZ_COUNT - 1) as f32) as u32;
        if t != sh.model.from_target.load(Ordering::Relaxed) {
            sh.model.from_target.store(t, Ordering::Relaxed);
            sh.model.store(&targets::kubyz_model(t as usize));
        }
        let g = sh.model.gen.load(Ordering::Acquire);
        if g != self.model_gen {
            self.model_gen = g;
            self.engine.model = sh.model.load();
        }
        if sh.capture_request.swap(false, Ordering::AcqRel) && self.engine.capture() {
            sh.captured.store(&self.engine.captured);
            slog!("captured f0={:.1} voiced={}", self.engine.captured.f0, self.engine.captured.harm_valid);
        }
        let g = sh.captured.gen.load(Ordering::Acquire);
        if g != self.captured_gen {
            self.captured_gen = g;
            self.engine.captured = sh.captured.load();
        }
    }

    fn publish_live(&self) {
        let (e, l) = (&self.engine, &self.shared.live);
        l.f0.store(e.frame.f0, Ordering::Relaxed);
        l.voiced.store(e.frame.voiced, Ordering::Relaxed);
        let mode = match e.mode {
            FrameMode::Pass => 0,
            FrameMode::Harmonic => 1,
            FrameMode::Envelope => 2,
        };
        l.mode.store(mode, Ordering::Relaxed);
        l.sc_f0.store(e.frame_sc.f0, Ordering::Relaxed);
        l.sc_voiced.store(e.sc_voiced(), Ordering::Relaxed);
        l.sc_level_db.store(e.frame_sc.level_db, Ordering::Relaxed);
        l.n.store(e.gains.show_n as u32, Ordering::Relaxed);
        for k in 0..MAX_HARM {
            l.voice[k].store(e.gains.show_voice[k], Ordering::Relaxed);
            l.target[k].store(e.gains.show_target[k], Ordering::Relaxed);
            l.out[k].store(e.gains.show_out[k], Ordering::Relaxed);
        }
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
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
                in_place_pair: None,
            }),
            // The target: any audio routed here (REAPER: track channels 3/4).
            (1, true) => writer.set(&AudioPortInfo {
                id: ClapId::new(1),
                name: b"Target",
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
    fn count(&mut self) -> u32 {
        PARAMS.len() as u32
    }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info_stepped(PARAMS, idx, info, STEPPED_PARAMS);
    }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        self.shared.params.get(id.get() as usize).map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(&mut self, id: ClapId, v: f64, w: &mut ParamDisplayWriter) -> core::fmt::Result {
        use core::fmt::Write;
        let names: Option<&[&str]> = match id.get() as usize {
            P_SOURCE => Some(&SOURCE_NAMES),
            P_MODE => Some(&MODE_NAMES),
            P_TARGET => Some(&targets::KUBYZ_NAMES),
            _ => None,
        };
        match names.and_then(|n| n.get(v.round().max(0.0) as usize)) {
            Some(name) => w.write_str(name),
            None => ParamDef::write_display(PARAMS, id, v, w),
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

impl PluginStateImpl for PluginMainThread<'_> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        use std::io::Write;
        output
            .write_all(self.shared.save_state_json().as_bytes())
            .map_err(|_| PluginError::Message("state write error"))
    }
    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        use std::io::Read;
        let mut s = String::new();
        input.read_to_string(&mut s).map_err(|_| PluginError::Message("state read error"))?;
        self.shared.load_state_json(&s).map_err(|e| {
            slog!("state load failed: {e}");
            PluginError::Message("state JSON error")
        })
    }
}

impl clack_extensions::latency::PluginLatencyImpl for PluginMainThread<'_> {
    fn get(&mut self) -> u32 {
        self.shared.latency_samples.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// CLAP GUI extension
// ---------------------------------------------------------------------------

use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window as ClapGuiWindow,
};

impl PluginGuiImpl for PluginMainThread<'_> {
    fn is_api_supported(&mut self, c: GuiConfiguration) -> bool {
        !c.is_floating
            && (c.api_type == GuiApiType::COCOA || c.api_type == GuiApiType::WIN32 || c.api_type == GuiApiType::X11)
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
        self.gui_handle = Some(gui::open_window(&window, self.shared.shared_handle(), self.gui_resize.clone()));
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

pub struct SuperDuperMorph;

impl Plugin for SuperDuperMorph {
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

impl DefaultPluginFactory for SuperDuperMorph {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("co.superduperai.morph", plugin_display_name!("SuperDuper Morph"))
            .with_vendor("SuperDuperAI")
            .with_version(version_string!("0.1"))
            .with_description("Timbre morph: the voice keeps its performance, its timbre moves toward a sidechain, a capture or the Kubyz model.")
            .with_features([AUDIO_EFFECT, STEREO, PITCH_SHIFTER])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Morph — build {} ({})", build_num!(), build_date!());
        let shared = PluginShared::new();
        superduper_synth_core::gui::spawn_bus_publisher_mcp(
            "SuperDuper Morph",
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

clack_export_entry!(SinglePluginEntry<SuperDuperMorph>);

impl clack_extensions::track_info::PluginTrackInfoImpl for PluginMainThread<'_> {
    fn changed(&self) {
        self.track_watch.refresh();
    }
}
