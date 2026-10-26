//! SuperDuper Denoise — removes broadband noise or room reverb from a voice or a whole mix.
//!
//! A thin CLAP wrapper over `superduper-restore` (the DSP lives there, without the plugin stack, so video
//! tools run the same algorithms offline). One `Mode` param picks the algorithm:
//!
//! - **Spectral** — STFT spectral gating. Adaptive by default (minimum-statistics floor, no learn
//!   step); flip **Learn** on while only noise plays and off again to freeze a noise profile,
//!   which then keeps held notes and steady tones the adaptive floor would eat.
//! - **Dereverb** — late-reverb suppression for a voice in a room (assumed RT60 knob).
//! - **Neural** — RNNoise (pure-Rust `nnnoiseless`, model compiled in). It is trained at 48 kHz
//!   only, so at any other host rate it refuses: exact passthrough, and the GUI says why.
//!   Decided over resampling: a resampler pair would add latency and its own artefacts to
//!   every mode's shared delay, and DAW video sessions run at 48 kHz anyway.
//!
//! **Mix** blends the latency-aligned dry signal back in, in every mode.
//! Every mode reports the same latency (the FFT size, ~43 ms at 48 kHz) through the CLAP
//! `latency` extension, so switching modes never moves the track against the others.
//! The learned Spectral profile is saved with the session (plugin state), per sample rate.

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod presets;

use atomic_float::AtomicF32;
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams,
    PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_common::stream::{InputStream, OutputStream};

use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use superduper_dsp_sdk::clap_helpers::{ParamDef, SimpleState};
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_restore::{Mode, Restorer, Settings};

fn init_logging() { superduper_dsp_sdk::log::init("denoise"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Param table
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    // 0 = Spectral, 1 = Dereverb, 2 = Neural (reserved, passthrough).
    ParamDef { id: 0, name: b"Mode",      min: 0.0, max: 2.0,   default: 0.0,  unit: "" },
    ParamDef { id: 1, name: b"Reduction", min: 0.0, max: 40.0,  default: 12.0, unit: "dB" },
    ParamDef { id: 2, name: b"Threshold", min: 0.0, max: 20.0,  default: 6.0,  unit: "dB" },
    ParamDef { id: 3, name: b"Smoothing", min: 0.0, max: 100.0, default: 50.0, unit: "%" },
    ParamDef { id: 4, name: b"Amount",    min: 0.0, max: 100.0, default: 50.0, unit: "%" },
    ParamDef { id: 5, name: b"RT60",      min: 0.1, max: 5.0,   default: 0.8,  unit: "s" },
    // 1 while the noise-only passage plays; back to 0 freezes the profile.
    ParamDef { id: 6, name: b"Learn",     min: 0.0, max: 1.0,   default: 0.0,  unit: "" },
    ParamDef { id: 7, name: b"Output",    min: -24.0, max: 12.0, default: 0.0, unit: "dB" },
    // Processed share: 100 = fully restored, lower lets some of the dry signal through.
    ParamDef { id: 8, name: b"Mix",       min: 0.0, max: 100.0, default: 100.0, unit: "%" },
];

const STEPPED_PARAMS: &[u32] = &[0, 6];

pub const P_MODE: usize = 0;
pub const P_REDUCTION: usize = 1;
pub const P_THRESHOLD: usize = 2;
pub const P_SMOOTHING: usize = 3;
pub const P_AMOUNT: usize = 4;
pub const P_RT60: usize = 5;
pub const P_LEARN: usize = 6;
pub const P_OUTPUT: usize = 7;
pub const P_MIX: usize = 8;

pub const MODE_NAMES: [&str; 3] = Mode::NAMES;

/// The `Settings` a param snapshot asks for — the one mapping, shared with offline renders.
pub fn settings_from(load: impl Fn(usize) -> f32) -> Settings {
    Settings {
        mode: Mode::from_index(load(P_MODE).round().max(0.0) as u32),
        reduction_db: load(P_REDUCTION),
        threshold_db: load(P_THRESHOLD),
        smoothing_pct: load(P_SMOOTHING),
        amount_pct: load(P_AMOUNT),
        rt60_s: load(P_RT60),
        mix_pct: load(P_MIX),
    }
}

// ---------------------------------------------------------------------------
// Shared params
// ---------------------------------------------------------------------------

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
    /// Samples, written on activate (it depends on the sample rate).
    pub latency: AtomicU32,
    /// Whether a learned profile is in use, for the GUI.
    pub has_profile: AtomicBool,
    /// False when Neural cannot run at the host's sample rate (it then passes through).
    pub neural_ready: AtomicBool,
    /// The learned profile as last frozen or loaded, what `save` writes.
    pub saved_profile: parking_lot::Mutex<Option<SavedProfile>>,
    /// A profile loaded from state, waiting for the audio thread to take it (no allocation there).
    pub incoming_profile: parking_lot::Mutex<Option<SavedProfile>>,
}

/// A learned Spectral profile with the sample rate it belongs to (its bin count depends on it).
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct SavedProfile {
    pub sample_rate: f32,
    pub bins: Vec<Vec<f32>>,
}

/// Plugin state: the common params block plus the learned profile. States without the profile
/// field (none were ever written, but the default keeps it tolerant) load as adaptive.
#[derive(serde::Serialize, serde::Deserialize)]
struct DenoiseState {
    #[serde(flatten)]
    base: SimpleState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<SavedProfile>,
}

pub struct PluginShared { pub inner: SharedParams }

impl PluginShared {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| AtomicBool::new(false)),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(4096),
                active_preset: AtomicU32::new(0),
                latency: AtomicU32::new(2048),
                has_profile: AtomicBool::new(false),
                neural_ready: AtomicBool::new(true),
                saved_profile: parking_lot::Mutex::new(None),
                incoming_profile: parking_lot::Mutex::new(None),
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
    restorer: Restorer,
    output: OutputGain,
    sample_rate: f32,
}

/// The Output knob: a 5 ms one-pole slew in dB (the `SmoothedParam` curve), with the
/// coefficient computed once and `powf` per sample only while the knob is moving.
struct OutputGain {
    db: f32,
    coef: f32,
}

impl OutputGain {
    fn new(db: f32, sample_rate: f32) -> Self {
        OutputGain { db, coef: (-1.0 / (0.005 * sample_rate)).exp() }
    }

    fn apply(&mut self, target_db: f32, channels: &mut [&mut [f32]]) {
        let len = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        if (self.db - target_db).abs() < 1e-4 {
            self.db = target_db;
            let g = 10f32.powf(target_db / 20.0);
            if g != 1.0 {
                channels.iter_mut().for_each(|c| c.iter_mut().for_each(|v| *v *= g));
            }
            return;
        }
        for i in 0..len {
            self.db = target_db + (self.db - target_db) * self.coef;
            let g = 10f32.powf(self.db / 20.0);
            channels.iter_mut().for_each(|c| c[i] *= g);
        }
    }
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
        shared.scope.set_sample_rate(cfg.sample_rate as f32);
        let sr = cfg.sample_rate as f32;
        let mut restorer = Restorer::new(sr, 2);
        shared.latency.store(restorer.latency() as u32, Ordering::Relaxed);
        shared.neural_ready.store(restorer.neural_ready(), Ordering::Relaxed);
        // A profile from the session (or from before a sample-rate change) comes back here,
        // off the audio thread; one learned at another rate is dropped by its bin count.
        shared.incoming_profile.lock().take();
        if let Some(p) = shared.saved_profile.lock().as_ref().filter(|p| p.sample_rate == sr) {
            restorer.denoiser_mut().set_profile(p.bins.clone());
        }
        shared.has_profile.store(restorer.denoiser().has_profile(), Ordering::Relaxed);
        slog!("activate sr={} latency={}", sr, restorer.latency());
        Ok(Self {
            shared,
            restorer,
            output: OutputGain::new(shared.params[P_OUTPUT].load(Ordering::Relaxed), sr),
            sample_rate: sr,
        })
    }

    fn reset(&mut self) {
        self.restorer.reset();
    }

    fn process(
        &mut self,
        process: Process,
        mut audio: Audio,
        mut events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let shared = self.shared;
        let _denormals = superduper_synth_core::effect::begin_events(&**shared, &process, &mut events);

        let sr = self.sample_rate;
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        self.restorer.set(settings_from(load));
        let learn = load(P_LEARN) >= 0.5;
        if learn != self.restorer.denoiser().is_learning() {
            self.restorer.learn(learn);
            if !learn {
                // A user action, once per learn: the copy for `save` may allocate here.
                if let (Some(bins), Some(mut slot)) = (self.restorer.denoiser().profile(), shared.saved_profile.try_lock()) {
                    *slot = Some(SavedProfile { sample_rate: sr, bins });
                }
            }
            shared.has_profile.store(self.restorer.denoiser().has_profile(), Ordering::Relaxed);
        }
        if let Some(p) = shared.incoming_profile.try_lock().and_then(|mut s| s.take()) {
            if p.sample_rate == sr {
                self.restorer.denoiser_mut().set_profile(p.bins);
            }
            shared.has_profile.store(self.restorer.denoiser().has_profile(), Ordering::Relaxed);
        }
        let output_db_t = load(P_OUTPUT);

        let (restorer, output) = (&mut self.restorer, &mut self.output);
        superduper_synth_core::effect::run_ports(&**shared, &mut audio, |l_read, l_write, r| {
            l_write.copy_from_slice(l_read);
            match r {
                Some((r_read, r_write)) => {
                    r_write.copy_from_slice(r_read);
                    let mut ch = [&mut *l_write, &mut *r_write];
                    restorer.process(&mut ch);
                    output.apply(output_db_t, &mut ch);
                }
                None => {
                    let mut ch = [&mut *l_write];
                    restorer.process(&mut ch);
                    output.apply(output_db_t, &mut ch);
                }
            }
        })?;
        Ok(ProcessStatus::Continue)
    }
}

impl PluginLatencyImpl for PluginMainThread<'_> {
    fn get(&mut self) -> u32 {
        self.shared.latency.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 { 1 }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index != 0 { return; }
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

impl PluginMainThread<'_> {
    /// What a parameter value reads as — the one place for it, so the host's
    /// text entry (`text_to_value`) accepts exactly what the display shows.
    fn display_text<W: core::fmt::Write>(&mut self, id: ClapId, v: f64, w: &mut W) -> core::fmt::Result {
        ParamDef::write_display(PARAMS, id, v, w)
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

impl SharedParamsInner {
    /// The session state as JSON: params, bypass, preset, bus key and the learned profile.
    pub fn save_state_json(&self) -> String {
        let state = DenoiseState {
            base: SimpleState::capture(
                &self.params,
                self.bypass.load(Ordering::Relaxed),
                self.active_preset.load(Ordering::Relaxed),
                &self.scope.key_track.lock(),
            ),
            profile: self.saved_profile.lock().clone(),
        };
        serde_json::to_string(&state).unwrap_or_default()
    }

    /// Restore from `save_state_json`; the profile is queued for the audio thread.
    pub fn load_state_json(&self, json: &str) -> Result<(), &'static str> {
        let state: DenoiseState = serde_json::from_str(json).map_err(|_| "state JSON read error")?;
        state.base.restore_params(&self.params)?;
        self.bypass.store(state.base.bypass, Ordering::Relaxed);
        self.active_preset.store(state.base.active_preset, Ordering::Relaxed);
        *self.scope.key_track.lock() = state.base.key_track;
        *self.saved_profile.lock() = state.profile.clone();
        *self.incoming_profile.lock() = state.profile;
        Ok(())
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
        let mut json = String::new();
        input.read_to_string(&mut json).map_err(|_| PluginError::Message("state read error"))?;
        self.shared.load_state_json(&json).map_err(PluginError::Message)?;
        self.track_watch.rescan_param_values();
        Ok(())
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
            width: self.gui_resize.0.load(Ordering::Relaxed),
            height: self.gui_resize.1.load(Ordering::Relaxed),
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
        self.gui_resize.0.store(w, Ordering::Relaxed);
        self.gui_resize.1.store(h, Ordering::Relaxed);
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

pub struct SuperDuperDenoise;

impl Plugin for SuperDuperDenoise {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginLatency>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::track_info::PluginTrackInfo>();
    }
}

impl DefaultPluginFactory for SuperDuperDenoise {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.denoise",
            plugin_display_name!("SuperDuper Denoise"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Noise and room-reverb remover: spectral gating (adaptive or learned) and dereverb")
        .with_features([AUDIO_EFFECT, STEREO, RESTORATION])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Denoise — build {} ({})", build_num!(), build_date!());
        let shared = PluginShared::new();
        superduper_synth_core::gui::spawn_bus_publisher_mcp("SuperDuper Denoise", &shared.inner, |s| &s.scope, Some(superduper_synth_core::gui::ParamsAccess { defs: PARAMS, values: |s| &s.params, dirty: Some(|s| &s.dirty_params) }), None);
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

clack_export_entry!(SinglePluginEntry<SuperDuperDenoise>);

impl clack_extensions::track_info::PluginTrackInfoImpl for PluginMainThread<'_> {
    fn changed(&self) {
        self.track_watch.refresh();
    }
}
