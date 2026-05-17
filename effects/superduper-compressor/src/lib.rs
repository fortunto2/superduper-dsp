//! SuperDuper Compressor — feed-forward, soft-knee, look-ahead, parallel mix.
//!
//! Design follows Giannoulis-Massberg-Reiss "Digital Dynamic Range
//! Compressor Design — A Tutorial and Analysis" (JAES 2012):
//!
//! ```text
//!   sidechain = max(|L|, |R|) → HPF (optional) → peak detector
//!                                                     │
//!                                                     ↓
//!                                  static curve (threshold/ratio/knee → dB cut)
//!                                                     │
//!                                                     ↓
//!                            attack/release smoothing (one-pole, asymmetric)
//!                                                     │
//!                                                     ↓
//!                                              gain_lin = 10^(reduction_dB/20)
//!                                                     │
//!   in_L,R → lookahead delay (2 ms) ────×────────────┘
//!                                       │
//!                                  makeup ×
//!                                       │
//!                          mix with dry → out_L,R
//! ```
//!
//! Stereo linked — gain reduction is the max of either channel's contribution,
//! applied to BOTH channels. Preserves the stereo image while still ducking
//! when only one channel peaks.

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
use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{
    compressor_gain_db, DelayLine, EnvelopeDetector, OnePoleLp, SmoothedParam,
};

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn log_path() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".superduper-dsp")
        .join("compressor.log")
}

static LOG_FILE: std::sync::OnceLock<parking_lot::Mutex<Option<std::fs::File>>> =
    std::sync::OnceLock::new();

fn init_logging() {
    LOG_FILE.get_or_init(|| {
        let path = log_path();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let file = std::fs::OpenOptions::new()
            .create(true).append(true).open(&path).ok();
        parking_lot::Mutex::new(file)
    });
}

fn slog_args(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    if let Some(slot) = LOG_FILE.get() {
        if let Some(file) = slot.lock().as_mut() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis()).unwrap_or(0);
            let _ = writeln!(file, "[{}] {}", now, args);
        }
    }
}

macro_rules! slog { ($($arg:tt)*) => { $crate::slog_args(format_args!($($arg)*)) } }

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Threshold", min: -60.0, max: 0.0,    default: -18.0, unit: "dB" },
    ParamDef { id: 1, name: b"Ratio",     min: 1.0,   max: 20.0,   default: 4.0,   unit: ":1" },
    ParamDef { id: 2, name: b"Attack",    min: 0.1,   max: 100.0,  default: 10.0,  unit: "ms" },
    ParamDef { id: 3, name: b"Release",   min: 5.0,   max: 1000.0, default: 120.0, unit: "ms" },
    ParamDef { id: 4, name: b"Knee",      min: 0.0,   max: 18.0,   default: 6.0,   unit: "dB" },
    ParamDef { id: 5, name: b"Makeup",    min: -12.0, max: 24.0,   default: 0.0,   unit: "dB" },
    // 0 = off, 1 = 80 Hz, 2 = 150 Hz, 3 = 300 Hz
    ParamDef { id: 6, name: b"SC HPF",    min: 0.0,   max: 3.0,    default: 1.0,   unit: ""   },
    ParamDef { id: 7, name: b"Mix",       min: 0.0,   max: 1.0,    default: 1.0,   unit: ""   },
    // Auto-release tracks how long compression has been sustained and
    // stretches the release accordingly — fast on transients, slower on
    // sustained material. Classic FabFilter Pro-C / Waves SSL behavior.
    ParamDef { id: 8, name: b"Auto Rel",  min: 0.0,   max: 1.0,    default: 0.0,   unit: ""   },
];

pub const P_THRESHOLD: usize = 0;
pub const P_RATIO: usize = 1;
pub const P_ATTACK: usize = 2;
pub const P_RELEASE: usize = 3;
pub const P_KNEE: usize = 4;
pub const P_MAKEUP: usize = 5;
pub const P_SC_HPF: usize = 6;
pub const P_MIX: usize = 7;
pub const P_AUTO_REL: usize = 8;

fn sc_hpf_hz(idx: u32) -> Option<f32> {
    match idx {
        1 => Some(80.0),
        2 => Some(150.0),
        3 => Some(300.0),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Shared params + GR meter atom (audio thread writes, GUI reads)
// ---------------------------------------------------------------------------

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: std::sync::atomic::AtomicBool,
    /// Current gain reduction in dB (always <= 0). Updated each block.
    pub gain_reduction_db: AtomicF32,
    /// Plugin latency in samples — written by `activate()` once sample
    /// rate is known, read by the CLAP latency extension's `get()` impl.
    /// Constant across the active session because our lookahead is fixed
    /// at compile time.
    pub latency_samples: std::sync::atomic::AtomicU32,
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
                gain_reduction_db: AtomicF32::new(0.0),
                latency_samples: std::sync::atomic::AtomicU32::new(0),
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

const LOOKAHEAD_MS: f32 = 2.0;

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    detector: EnvelopeDetector,
    /// HPF on the sidechain signal so low-end doesn't pump the comp.
    sc_hpf_state: f32,
    /// Lookahead buffers — main signal is delayed so we can react before
    /// the transient hits the gain stage. 2 ms typical.
    look_l: DelayLine,
    look_r: DelayLine,
    lookahead_samples: f32,
    /// Scratch for the external sidechain port. When routed (host sends
    /// non-zero audio into port 1), this replaces |L|·|R| as the detector
    /// source — classic "kick triggers bass duck" pattern.
    sc_buf_l: Box<[f32]>,
    sc_buf_r: Box<[f32]>,
    smooth_threshold: SmoothedParam,
    smooth_ratio: SmoothedParam,
    smooth_attack: SmoothedParam,
    smooth_release: SmoothedParam,
    smooth_knee: SmoothedParam,
    smooth_makeup: SmoothedParam,
    smooth_mix: SmoothedParam,
    /// Used by the GUI meter — peak-hold smoothing so the bar doesn't flicker.
    meter_smooth: f32,
    /// Slow envelope tracking how sustained the current compression is.
    /// Drives the program-dependent release multiplier.
    sustained_gr_env: f32,
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
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        slog!("activate sr={}", sr);
        let look_samples = sr * 0.001 * LOOKAHEAD_MS;
        let cap = (look_samples as usize + 64).next_power_of_two().max(1024);
        let max_frames = audio_config.max_frames_count as usize;

        // Report our latency to the host once we know SR. This is the
        // amount of delay the lookahead delay-line introduces, in samples.
        shared
            .latency_samples
            .store(look_samples as u32, Ordering::Relaxed);

        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        Ok(Self {
            shared,
            detector: EnvelopeDetector::default(),
            sc_hpf_state: 0.0,
            look_l: DelayLine::new(cap),
            look_r: DelayLine::new(cap),
            lookahead_samples: look_samples,
            sc_buf_l: vec![0.0; max_frames].into_boxed_slice(),
            sc_buf_r: vec![0.0; max_frames].into_boxed_slice(),
            smooth_threshold: SmoothedParam::new(load(P_THRESHOLD)),
            smooth_ratio: SmoothedParam::new(load(P_RATIO)),
            smooth_attack: SmoothedParam::new(load(P_ATTACK)),
            smooth_release: SmoothedParam::new(load(P_RELEASE)),
            smooth_knee: SmoothedParam::new(load(P_KNEE)),
            smooth_makeup: SmoothedParam::new(load(P_MAKEUP)),
            smooth_mix: SmoothedParam::new(load(P_MIX)),
            meter_smooth: 0.0,
            sustained_gr_env: 0.0,
            sample_rate: sr,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        apply_param_events(self.shared, events.input);

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        let sr = self.sample_rate;

        let threshold_target = self.shared.params[P_THRESHOLD].load(Ordering::Relaxed);
        let ratio_target = self.shared.params[P_RATIO].load(Ordering::Relaxed);
        let attack_target = self.shared.params[P_ATTACK].load(Ordering::Relaxed);
        let release_target = self.shared.params[P_RELEASE].load(Ordering::Relaxed);
        let knee_target = self.shared.params[P_KNEE].load(Ordering::Relaxed);
        let makeup_target = self.shared.params[P_MAKEUP].load(Ordering::Relaxed);
        let mix_target = self.shared.params[P_MIX].load(Ordering::Relaxed);
        let sc_hpf_idx = self.shared.params[P_SC_HPF]
            .load(Ordering::Relaxed)
            .round() as u32;
        let sc_hpf_freq = sc_hpf_hz(sc_hpf_idx);
        let auto_rel = self.shared.params[P_AUTO_REL].load(Ordering::Relaxed) > 0.5;

        let mut max_gr_db: f32 = 0.0;

        // ---- Snapshot the sidechain port (index 1) before touching main ----
        let frames = audio.frames_count() as usize;
        let n_frames = frames.min(self.sc_buf_l.len());
        let mut sc_present = false;
        if let Some(sc_port) = audio.input_port(1) {
            if let Some(chans) = sc_port.channels()?.into_f32() {
                if let Some(l) = chans.channel(0) {
                    let n = n_frames.min(l.len());
                    self.sc_buf_l[..n].copy_from_slice(&l[..n]);
                    if l.iter().take(n).any(|&x| x != 0.0) { sc_present = true; }
                }
                if let Some(r) = chans.channel(1) {
                    let n = n_frames.min(r.len());
                    self.sc_buf_r[..n].copy_from_slice(&r[..n]);
                    if r.iter().take(n).any(|&x| x != 0.0) { sc_present = true; }
                } else {
                    self.sc_buf_r[..n_frames].copy_from_slice(&self.sc_buf_l[..n_frames]);
                }
            }
        }

        // ---- Process main port (index 0) ----
        if let Some(mut main_pair) = audio.port_pair(0) {
            let Some(channel_pairs) = main_pair.channels()?.into_f32() else {
                return Ok(ProcessStatus::Continue);
            };
            let mut iter = channel_pairs.into_iter();
            let Some(ch_l) = iter.next() else { return Ok(ProcessStatus::Continue); };
            let ch_r = iter.next();

            use superduper_dsp_sdk::clap_helpers::split_io;
            let Some((l_read, l_write)) = split_io(ch_l) else {
                return Ok(ProcessStatus::Continue);
            };
            let r = ch_r.and_then(split_io);

            if bypassed {
                l_write.copy_from_slice(l_read);
                if let Some((rr, rw)) = r { rw.copy_from_slice(rr); }
                self.shared.gain_reduction_db.store(0.0, Ordering::Relaxed);
                return Ok(ProcessStatus::Continue);
            }

            if let Some((r_read, r_write)) = r {
                process_stereo_block(
                    self, l_read, l_write, r_read, r_write, sr,
                    threshold_target, ratio_target, attack_target, release_target,
                    knee_target, makeup_target, mix_target,
                    sc_hpf_freq, sc_present, auto_rel,
                    &mut max_gr_db,
                );
            } else {
                let n = l_read.len();
                for i in 0..n {
                    let dry = l_read[i];
                    let (out, gr) = process_sample_mono(
                        &mut self.detector, &mut self.sc_hpf_state, sc_hpf_freq,
                        &mut self.look_l, self.lookahead_samples,
                        &mut self.smooth_threshold, &mut self.smooth_ratio,
                        &mut self.smooth_attack, &mut self.smooth_release,
                        &mut self.smooth_knee, &mut self.smooth_makeup,
                        &mut self.smooth_mix,
                        dry, sr,
                        threshold_target, ratio_target, attack_target, release_target,
                        knee_target, makeup_target, mix_target,
                    );
                    l_write[i] = out;
                    if gr < max_gr_db { max_gr_db = gr; }
                }
            }
        }

        // Push the strongest GR seen this block out for the GUI meter.
        // Peak-hold w/ slow release so the bar reads ~max recent value.
        let release_coef = (-1.0 / (0.15 * sr)).exp(); // 150 ms decay
        if max_gr_db < self.meter_smooth {
            self.meter_smooth = max_gr_db;
        } else {
            self.meter_smooth = max_gr_db + (self.meter_smooth - max_gr_db) * release_coef;
        }
        self.shared
            .gain_reduction_db
            .store(self.meter_smooth, Ordering::Relaxed);

        Ok(ProcessStatus::Continue)
    }
}

#[allow(clippy::too_many_arguments)]
fn process_stereo_block(
    p: &mut PluginAudioProcessor<'_>,
    l_read: &[f32],
    l_write: &mut [f32],
    r_read: &[f32],
    r_write: &mut [f32],
    sr: f32,
    threshold_target: f32, ratio_target: f32, attack_target: f32, release_target: f32,
    knee_target: f32, makeup_target: f32, mix_target: f32,
    sc_hpf_freq: Option<f32>, sc_present: bool, auto_rel: bool,
    max_gr_db: &mut f32,
) {
    let n = l_read.len().min(r_read.len());
    for i in 0..n {
        let dry_l = l_read[i];
        let dry_r = r_read[i];

        let threshold = p.smooth_threshold.step(threshold_target, sr);
        let ratio = p.smooth_ratio.step(ratio_target, sr);
        let attack = p.smooth_attack.step(attack_target, sr);
        let mut release = p.smooth_release.step(release_target, sr);
        let knee = p.smooth_knee.step(knee_target, sr);
        let makeup = p.smooth_makeup.step(makeup_target, sr);
        let mix = p.smooth_mix.step(mix_target, sr);

        // Program-dependent release: stretches base release by up to 4×
        // when the slow envelope shows we've been compressing heavily.
        // The slow envelope itself is a one-pole with a ~200 ms attack
        // and ~600 ms release on |gr_db|.
        if auto_rel {
            release *= 1.0 + 3.0 * p.sustained_gr_env.min(1.0);
        }

        // Detector source — external sidechain if routed, otherwise dry main.
        let (key_l, key_r) = if sc_present {
            (
                p.sc_buf_l.get(i).copied().unwrap_or(0.0),
                p.sc_buf_r.get(i).copied().unwrap_or(0.0),
            )
        } else {
            (dry_l, dry_r)
        };
        let mut sc = key_l.abs().max(key_r.abs());
        if let Some(hp_hz) = sc_hpf_freq {
            let coef = (-core::f32::consts::TAU * hp_hz / sr).exp();
            p.sc_hpf_state = sc * (1.0 - coef) + p.sc_hpf_state * coef;
            sc -= p.sc_hpf_state;
        }

        let env = p.detector.process(sc, sr, attack, release);
        let env_db = 20.0 * env.max(1e-9).log10();
        let gr_db = compressor_gain_db(env_db, threshold, ratio, knee);
        if gr_db < *max_gr_db { *max_gr_db = gr_db; }

        // Slow envelope on |gr_db|, normalised to 0..1 over the first 6 dB
        // of compression. Asymmetric: rises slowly (300 ms), falls slowly
        // (600 ms) — gives auto-release the program-dependent character.
        let gr_norm = (-gr_db / 6.0).clamp(0.0, 1.0);
        let coef = if gr_norm > p.sustained_gr_env {
            (-1.0 / (0.3 * sr)).exp()
        } else {
            (-1.0 / (0.6 * sr)).exp()
        };
        p.sustained_gr_env = gr_norm + (p.sustained_gr_env - gr_norm) * coef;

        let gain_lin = 10f32.powf((gr_db + makeup) / 20.0);

        p.look_l.write(dry_l);
        p.look_r.write(dry_r);
        let delayed_l = p.look_l.read_lagrange3(p.lookahead_samples);
        let delayed_r = p.look_r.read_lagrange3(p.lookahead_samples);

        let wet_l = delayed_l * gain_lin;
        let wet_r = delayed_r * gain_lin;

        l_write[i] = delayed_l * (1.0 - mix) + wet_l * mix;
        r_write[i] = delayed_r * (1.0 - mix) + wet_r * mix;
    }
}

#[allow(clippy::too_many_arguments)]
fn process_sample_mono(
    detector: &mut EnvelopeDetector,
    sc_hpf_state: &mut f32,
    sc_hpf_freq: Option<f32>,
    look: &mut DelayLine,
    lookahead_samples: f32,
    sm_thr: &mut SmoothedParam,
    sm_rat: &mut SmoothedParam,
    sm_atk: &mut SmoothedParam,
    sm_rel: &mut SmoothedParam,
    sm_knee: &mut SmoothedParam,
    sm_makeup: &mut SmoothedParam,
    sm_mix: &mut SmoothedParam,
    dry: f32, sr: f32,
    threshold_target: f32, ratio_target: f32, attack_target: f32, release_target: f32,
    knee_target: f32, makeup_target: f32, mix_target: f32,
) -> (f32, f32) {
    let threshold = sm_thr.step(threshold_target, sr);
    let ratio = sm_rat.step(ratio_target, sr);
    let attack = sm_atk.step(attack_target, sr);
    let release = sm_rel.step(release_target, sr);
    let knee = sm_knee.step(knee_target, sr);
    let makeup = sm_makeup.step(makeup_target, sr);
    let mix = sm_mix.step(mix_target, sr);

    let mut sc = dry.abs();
    if let Some(hp_hz) = sc_hpf_freq {
        let coef = (-core::f32::consts::TAU * hp_hz / sr).exp();
        *sc_hpf_state = sc * (1.0 - coef) + *sc_hpf_state * coef;
        sc -= *sc_hpf_state;
    }
    let env = detector.process(sc, sr, attack, release);
    let env_db = 20.0 * env.max(1e-9).log10();
    let gr_db = compressor_gain_db(env_db, threshold, ratio, knee);
    let total_db = gr_db + makeup;
    let gain_lin = 10f32.powf(total_db / 20.0);
    let _ = look;
    let _ = lookahead_samples;
    let wet = dry * gain_lin;
    (dry * (1.0 - mix) + wet * mix, gr_db)
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input { 2 } else { 1 } // main I/O + sidechain input
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
        ParamDef::write_info(PARAMS, idx, info);
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

pub struct SuperDuperCompressor;

impl Plugin for SuperDuperCompressor {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::latency::PluginLatency>();
    }
}

impl clack_extensions::latency::PluginLatencyImpl for PluginMainThread<'_> {
    fn get(&mut self) -> u32 {
        self.shared.latency_samples.load(Ordering::Relaxed)
    }
}

impl DefaultPluginFactory for SuperDuperCompressor {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.compressor",
            plugin_display_name!("SuperDuper Compressor"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Soft-knee feed-forward compressor with lookahead and sidechain HPF")
        .with_features([AUDIO_EFFECT, STEREO, COMPRESSOR])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Compressor — build {} ({})", build_num!(), build_date!());
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

// Touch OnePoleLp so unused-import lint doesn't fire — kept available for
// future tweaks of the sidechain HPF without changing imports.
#[allow(dead_code)]
fn _keep_onepole_referenced() {
    let _ = OnePoleLp::default();
}

clack_export_entry!(SinglePluginEntry<SuperDuperCompressor>);
