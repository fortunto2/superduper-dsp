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

pub mod detector; // Classic / Dual (RMS + peak) detector, bus key level
pub mod gui;
pub mod mail; // gain-reduction meter as a read-only bus-mailbox document
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
use superduper_dsp_sdk::clap_helpers::{ParamDef, SidechainSnapshot};
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{
    oversample_apply, CompressorCurve, DelayLine, Oversampler2x, SmoothedParam,
    OVERSAMPLE_LATENCY,
};

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn init_logging() { superduper_dsp_sdk::log::init("compressor"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0,  name: b"Threshold", min: -60.0, max: 0.0,    default: -18.0, unit: "dB" },
    ParamDef { id: 1,  name: b"Ratio",     min: 1.0,   max: 20.0,   default: 4.0,   unit: ":1" },
    ParamDef { id: 2,  name: b"Attack",    min: 0.1,   max: 100.0,  default: 10.0,  unit: "ms" },
    ParamDef { id: 3,  name: b"Release",   min: 5.0,   max: 1000.0, default: 120.0, unit: "ms" },
    ParamDef { id: 4,  name: b"Knee",      min: 0.0,   max: 18.0,   default: 6.0,   unit: "dB" },
    ParamDef { id: 5,  name: b"Makeup",    min: -12.0, max: 24.0,   default: 0.0,   unit: "dB" },
    // 0 = off, 1 = 80 Hz, 2 = 150 Hz, 3 = 300 Hz
    ParamDef { id: 6,  name: b"SC HPF",    min: 0.0,   max: 3.0,    default: 1.0,   unit: ""   },
    ParamDef { id: 7,  name: b"Mix",       min: 0.0,   max: 1.0,    default: 1.0,   unit: ""   },
    // Auto-release tracks how long compression has been sustained and
    // stretches the release accordingly — fast on transients, slower on
    // sustained material. Classic FabFilter Pro-C / Waves SSL behavior.
    ParamDef { id: 8,  name: b"Auto Rel",  min: 0.0,   max: 1.0,    default: 1.0,   unit: ""   },
    // Adjustable lookahead. Capped at 15 ms — beyond that the audible
    // pre-attenuation outweighs the transient catch. ZLCompressor caps
    // around 10 ms; we go a touch farther for "brickwall-ish" behavior.
    ParamDef { id: 9,  name: b"Lookahead", min: 0.0,   max: 15.0,   default: 2.0,   unit: "ms" },
    // Soft tanh ceiling on the output to catch overshoots after makeup.
    // 0 dB = effectively off (tanh saturates only at very large inputs).
    // Negative values progressively clamp peaks toward the chosen ceiling.
    ParamDef { id: 10, name: b"Ceiling",   min: -12.0, max: 0.0,    default: 0.0,   unit: "dB" },
    // Stereo coupling: 1.0 = fully linked (max of L/R drives both),
    // 0.0 = fully independent (each channel gets its own GR).
    ParamDef { id: 11, name: b"Link",      min: 0.0,   max: 1.0,    default: 1.0,   unit: ""   },
    // Oversampling for the ceiling clipper stage. 0 = Off (native),
    // 1 = 2×, 2 = 4× (two cascaded halfband stages). Aliasing produced
    // by tanh on loud signals folds back into the audible band at the
    // native rate; running the non-linearity at 2× or 4× pushes the
    // mirror images above Nyquist where the decimator stop-band
    // attenuates them by ~80 dB.
    ParamDef { id: 12, name: b"OS",        min: 0.0,   max: 2.0,    default: 0.0,   unit: ""   },
    // Static compression curve shape. 0 = Clean (Giannoulis quadratic),
    // 1 = Pump (asymmetric, +25% slope boost in the first 6 dB above
    // threshold), 2 = Smooth (cubic smoothstep knee). Discrete switch.
    ParamDef { id: 13, name: b"Curve",     min: 0.0,   max: 2.0,    default: 0.0,   unit: ""   },
    // Hard limit on maximum gain reduction. 0 dB = no clamp (the curve
    // freely takes the ratio above threshold). Anything >0 caps GR at
    // -Range, useful for "soft limit" and downward-expansion-style use.
    ParamDef { id: 14, name: b"Range",     min: 0.0,   max: 36.0,   default: 0.0,   unit: "dB" },
    // After the detector envelope falls below threshold, freeze the
    // current GR for `Hold` ms before the release stage begins. Same
    // knob ZLCompressor calls "Hold" — preserves sustain on slow material
    // while preventing pumping on transients with gaps.
    ParamDef { id: 15, name: b"Hold",      min: 0.0,   max: 500.0,  default: 0.0,   unit: "ms" },
    // Channel mode: 0 = Stereo (LR), 1 = M/S (encode → compress mid and
    // side independently → decode). Useful for mastering — keeps the
    // centred lead un-compressed while sides duck, or vice versa.
    ParamDef { id: 16, name: b"M/S",       min: 0.0,   max: 1.0,    default: 0.0,   unit: ""   },
    // Detector topology: 0 = Classic (one peak-ish envelope, everything
    // above), 1 = Dual (RMS program stage + peak stage, see detector.rs).
    // Default Classic so old projects and presets sound identical.
    ParamDef { id: 17, name: b"Detector",  min: 0.0,   max: 1.0,    default: 0.0,   unit: ""   },
    // Dual only: how far peaks may rise above the RMS-controlled level
    // before the peak stage acts. 0 = both stages share one threshold.
    ParamDef { id: 18, name: b"Peak Crest", min: 0.0,  max: 18.0,   default: 9.0,   unit: "dB" },
    // Dual only: recovery after short overloads (peak stage)…
    ParamDef { id: 19, name: b"Release Peak", min: 5.0, max: 500.0, default: 40.0,  unit: "ms" },
    // …and after sustained reduction (RMS stage). Classic uses "Release".
    ParamDef { id: 20, name: b"Release RMS", min: 20.0, max: 3000.0, default: 400.0, unit: "ms" },
    // Bus key: 0 = off (the detector hears the input / sidechain port),
    // 1 = the detector follows the bus key track's level only. The bus lags
    // 50–100 ms, so this is for slow ducking (music under a voice); the real
    // sidechain input stays the tool for kick pumping.
    ParamDef { id: 21, name: b"Key",       min: 0.0,   max: 1.0,    default: 0.0,   unit: ""   },
    // The band of the key track that counts (e.g. 200–4000 Hz = voice).
    ParamDef { id: 22, name: b"Key Lo",    min: 20.0,  max: 20000.0, default: 150.0, unit: "Hz" },
    ParamDef { id: 23, name: b"Key Hi",    min: 20.0,  max: 20000.0, default: 5000.0, unit: "Hz" },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[12, 13, 17];

pub const P_THRESHOLD: usize = 0;
pub const P_RATIO: usize = 1;
pub const P_ATTACK: usize = 2;
pub const P_RELEASE: usize = 3;
pub const P_KNEE: usize = 4;
pub const P_MAKEUP: usize = 5;
pub const P_SC_HPF: usize = 6;
pub const P_MIX: usize = 7;
pub const P_AUTO_REL: usize = 8;
pub const P_LOOKAHEAD: usize = 9;
pub const P_CEILING: usize = 10;
pub const P_LINK: usize = 11;
pub const P_OS: usize = 12;
pub const P_CURVE: usize = 13;
pub const P_RANGE: usize = 14;
pub const P_HOLD: usize = 15;
pub const P_MS_MODE: usize = 16;
pub const P_DETECTOR: usize = 17;
pub const P_PEAK_CREST: usize = 18;
pub const P_RELEASE_PEAK: usize = 19;
pub const P_RELEASE_RMS: usize = 20;
pub const P_KEY: usize = 21;
pub const P_KEY_LO: usize = 22;
pub const P_KEY_HI: usize = 23;

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

/// Length of the visual scope buffer (number of decimated points).
/// At 1500 Hz update rate (~32-sample hop @ 48 k) this is ~0.7 s of history.
pub const SCOPE_LEN: usize = 1024;

/// Triple-stream lock-free SPSC ring buffer used for the live oscilloscope:
/// the audio thread is the sole writer, the GUI is the sole reader. Each
/// slot stores one decimated sample's input peak, output peak and GR (all
/// in dBFS / dB respectively). We tolerate tearing — visual glitches are
/// preferable to taking a Mutex on the audio thread.
pub struct ScopeBuf {
    pub in_db: Box<[AtomicF32]>,
    pub out_db: Box<[AtomicF32]>,
    pub gr_db: Box<[AtomicF32]>,
    pub head: std::sync::atomic::AtomicU32,
}

impl ScopeBuf {
    pub fn new(len: usize) -> Self {
        let mk = |fill: f32| {
            (0..len).map(|_| AtomicF32::new(fill)).collect::<Vec<_>>().into_boxed_slice()
        };
        Self {
            in_db: mk(-72.0),
            out_db: mk(-72.0),
            gr_db: mk(0.0),
            head: std::sync::atomic::AtomicU32::new(0),
        }
    }

    /// Append one decimated frame. Called from the audio thread.
    #[inline]
    pub fn push(&self, in_db: f32, out_db: f32, gr_db: f32) {
        let len = self.in_db.len();
        let idx = (self.head.load(Ordering::Relaxed) as usize) % len;
        self.in_db[idx].store(in_db, Ordering::Relaxed);
        self.out_db[idx].store(out_db, Ordering::Relaxed);
        self.gr_db[idx].store(gr_db, Ordering::Relaxed);
        // Wrap head explicitly so the next write lands at the right slot.
        let next = ((idx + 1) % len) as u32;
        self.head.store(next, Ordering::Relaxed);
    }

    /// Read out a chronologically-ordered snapshot. Called from the GUI
    /// thread (~30 Hz). Mild tearing is OK — visual artifact is one row
    /// of stale dB values, no audio impact.
    pub fn snapshot_in_order(&self, out_in: &mut [f32], out_out: &mut [f32], out_gr: &mut [f32]) {
        let len = self.in_db.len();
        let head = self.head.load(Ordering::Relaxed) as usize;
        for i in 0..len.min(out_in.len().min(out_out.len()).min(out_gr.len())) {
            let src = (head + i) % len;
            out_in[i] = self.in_db[src].load(Ordering::Relaxed);
            out_out[i] = self.out_db[src].load(Ordering::Relaxed);
            out_gr[i] = self.gr_db[src].load(Ordering::Relaxed);
        }
    }
}

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: std::sync::atomic::AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub dirty_params: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_begin: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_end: [std::sync::atomic::AtomicBool; PARAMS.len()],
    /// Currently-selected preset index — persisted via simple_state
    /// so the dropdown survives project reopens.
    pub active_preset: std::sync::atomic::AtomicU32,
    /// Current gain reduction in dB (always <= 0). Updated each block.
    pub gain_reduction_db: AtomicF32,
    /// Dual detector: deepest GR of each stage in the last block (dB ≤ 0).
    /// Classic reports its whole GR as the RMS ("program") stage.
    pub gr_rms_db: AtomicF32,
    pub gr_peak_db: AtomicF32,
    /// Bus key level over Key Lo..Hi (dBFS RMS), −120 when no key is set.
    pub key_level_db: AtomicF32,
    /// Plugin latency in samples — written by `activate()` whenever the
    /// lookahead knob changes during the session, read by the CLAP
    /// latency extension's `get()` impl.
    pub latency_samples: std::sync::atomic::AtomicU32,
    /// Live waveform scope, written by the audio thread + read by GUI.
    /// In / out / GR meter history for the GUI (decimated peaks).
    pub meter: ScopeBuf,
    /// Raw output, mono — the same LiveScope every plugin has (spectrum
    /// strip, SuperDuper bus).
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
                bypass: std::sync::atomic::AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                active_preset: std::sync::atomic::AtomicU32::new(0),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                gain_reduction_db: AtomicF32::new(0.0),
                gr_rms_db: AtomicF32::new(0.0),
                gr_peak_db: AtomicF32::new(0.0),
                key_level_db: AtomicF32::new(-120.0),
                latency_samples: std::sync::atomic::AtomicU32::new(0),
                meter: ScopeBuf::new(SCOPE_LEN),
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

/// Upper bound on the Lookahead knob. Used to size the delay-line capacity
/// at activate() so we never reallocate from the audio thread when the user
/// drags the knob upward.
const MAX_LOOKAHEAD_MS: f32 = 15.0;
/// Decimation factor for the scope. Audio thread accumulates running peak
/// for this many samples then pushes one scope frame. 32 samples @ 48 kHz
/// = 1500 Hz update rate — fast enough that the GUI shows real transient
/// detail, slow enough that the atomic stores don't dominate runtime.
const SCOPE_HOP: usize = 32;

pub struct PluginMainThread<'a> {
    track_watch: superduper_synth_core::gui::TrackInfoWatch<'a>,
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    detector: detector::Detector,
    /// HPF on the **raw signed** sidechain audio so low-end (kick / boomy
    /// fundamentals) doesn't pump the comp. One state per channel; the
    /// filter has to sit BEFORE rectification — applying it to `|x|`
    /// only subtracts the DC mean of the rectifier output and crushes
    /// the detected envelope by 6-10 dB on broadband music (see
    /// `tests/sc_hpf_repro.rs`).
    sc_hpf_state_l: f32,
    sc_hpf_state_r: f32,
    /// Lookahead buffers — main signal is delayed so we can react before
    /// the transient hits the gain stage. 2 ms typical.
    look_l: DelayLine,
    look_r: DelayLine,
    lookahead_samples: f32,
    /// External sidechain scratch + routed latch. Once routed, the key
    /// replaces |L|·|R| as the detector source — classic "kick triggers bass
    /// duck" — and silence in it means "release", never "re-key off the
    /// bass" (see `SidechainSnapshot` in the sdk for the full story).
    sc: SidechainSnapshot,
    /// Taken out of the struct for the duration of the mono call so the
    /// borrow checker allows `&mut self` alongside the slices; put back
    /// right after. No allocation on the audio thread — take/replace move
    /// the same boxes.
    mono_r_in: Option<Box<[f32]>>,
    mono_r_out: Option<Box<[f32]>>,
    smooth_threshold: SmoothedParam,
    smooth_ratio: SmoothedParam,
    smooth_attack: SmoothedParam,
    smooth_release: SmoothedParam,
    smooth_knee: SmoothedParam,
    smooth_makeup: SmoothedParam,
    smooth_mix: SmoothedParam,
    smooth_lookahead: SmoothedParam,
    smooth_ceiling: SmoothedParam,
    smooth_link: SmoothedParam,
    /// Used by the GUI meter — peak-hold smoothing so the bar doesn't flicker.
    meter_smooth: f32,
    /// Slow envelope tracking how sustained the current compression is.
    /// Drives the program-dependent release multiplier.
    sustained_gr_env: f32,
    /// Hold-stage state — when the static curve says "release allowed"
    /// (input below threshold), we freeze the previous GR for this many
    /// remaining samples before letting the detector envelope cool down.
    hold_remaining_samples: f32,
    /// Latched GR value used while `hold_remaining_samples > 0`. Sample-
    /// accurate latch on the per-sample compressor output, not an extra
    /// smoothed value.
    hold_gr_db: f32,
    /// Running scope accumulator — collects peak input / peak output / max
    /// GR across SCOPE_HOP samples then emits one scope frame.
    scope_acc_in: f32,
    scope_acc_out: f32,
    scope_acc_gr: f32,
    scope_acc_count: usize,
    /// Halfband oversamplers for the ceiling clipper. Two cascaded stages
    /// per channel give 1×/2×/4× modes — only stage 1 is used at 2×, both
    /// at 4×. Aliasing avoidance is local to the non-linear clipper since
    /// the compressor curve itself is mostly linear and doesn't produce
    /// problematic intermod.
    os1_l: Oversampler2x,
    os1_r: Oversampler2x,
    os2_l: Oversampler2x,
    os2_r: Oversampler2x,
    sample_rate: f32,
    /// Deepest RMS-stage / peak-stage GR of the current block (meter doc).
    blk_gr_rms: f32,
    blk_gr_peak: f32,
}

/// Per-block settings of the Dual detector and the bus key, read once in
/// `process()` and passed to the sample loop as one value.
#[derive(Clone, Copy)]
struct DualBlock {
    dual: bool,
    crest_db: f32,
    release_peak_ms: f32,
    release_rms_ms: f32,
    /// Key blend 0..1 and the key level as a linear detector value.
    key_amt: f32,
    key_lin: f32,
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
        shared.scope.set_sample_rate(audio_config.sample_rate as f32);
        let sr = audio_config.sample_rate as f32;
        slog!("activate sr={}", sr);
        // Size the delay line for the max-knob position so we never have
        // to re-allocate on the audio thread when the user pushes Lookahead.
        let max_look_samples = sr * 0.001 * MAX_LOOKAHEAD_MS;
        let cap = (max_look_samples as usize + 64).next_power_of_two().max(1024);
        let max_frames = audio_config.max_frames_count as usize;

        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        let initial_look_ms = load(P_LOOKAHEAD).max(0.0);
        let initial_look_samples = sr * 0.001 * initial_look_ms;
        shared
            .latency_samples
            .store(initial_look_samples as u32, Ordering::Relaxed);

        Ok(Self {
            shared,
            detector: detector::Detector::default(),
            sc_hpf_state_l: 0.0,
            sc_hpf_state_r: 0.0,
            look_l: DelayLine::new(cap),
            look_r: DelayLine::new(cap),
            lookahead_samples: initial_look_samples,
            sc: SidechainSnapshot::new(max_frames),
            // Mono runs through the SAME stereo block with L copied here —
            // process_sample_mono was a second implementation that had to
            // learn every feature one forgotten port at a time (Range was
            // the fifth; curve/hold/sidechain/auto-release never made it).
            mono_r_in: Some(vec![0.0; max_frames].into_boxed_slice()),
            mono_r_out: Some(vec![0.0; max_frames].into_boxed_slice()),
            smooth_threshold: SmoothedParam::new(load(P_THRESHOLD)),
            smooth_ratio: SmoothedParam::new(load(P_RATIO)),
            smooth_attack: SmoothedParam::new(load(P_ATTACK)),
            smooth_release: SmoothedParam::new(load(P_RELEASE)),
            smooth_knee: SmoothedParam::new(load(P_KNEE)),
            smooth_makeup: SmoothedParam::new(load(P_MAKEUP)),
            smooth_mix: SmoothedParam::new(load(P_MIX)),
            smooth_lookahead: SmoothedParam::new(load(P_LOOKAHEAD)),
            smooth_ceiling: SmoothedParam::new(load(P_CEILING)),
            smooth_link: SmoothedParam::new(load(P_LINK)),
            meter_smooth: 0.0,
            sustained_gr_env: 0.0,
            scope_acc_in: 0.0,
            scope_acc_out: 0.0,
            scope_acc_gr: 0.0,
            scope_acc_count: 0,
            os1_l: Oversampler2x::default(),
            os1_r: Oversampler2x::default(),
            os2_l: Oversampler2x::default(),
            os2_r: Oversampler2x::default(),
            hold_remaining_samples: 0.0,
            hold_gr_db: 0.0,
            sample_rate: sr,
            blk_gr_rms: 0.0,
            blk_gr_peak: 0.0,
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
        let sr = self.sample_rate;

        let threshold_target = self.shared.params[P_THRESHOLD].load(Ordering::Relaxed);
        let ratio_target = self.shared.params[P_RATIO].load(Ordering::Relaxed);
        let attack_target = self.shared.params[P_ATTACK].load(Ordering::Relaxed);
        let release_target = self.shared.params[P_RELEASE].load(Ordering::Relaxed);
        let knee_target = self.shared.params[P_KNEE].load(Ordering::Relaxed);
        let makeup_target = self.shared.params[P_MAKEUP].load(Ordering::Relaxed);
        let mix_target = self.shared.params[P_MIX].load(Ordering::Relaxed);
        let lookahead_ms_target = self
            .shared.params[P_LOOKAHEAD].load(Ordering::Relaxed).max(0.0);
        let ceiling_target = self.shared.params[P_CEILING].load(Ordering::Relaxed);
        let link_target = self.shared.params[P_LINK].load(Ordering::Relaxed).clamp(0.0, 1.0);
        // Oversampling mode is a discrete switch — round and clamp.
        let os_mode = self.shared.params[P_OS]
            .load(Ordering::Relaxed)
            .round()
            .clamp(0.0, 2.0) as u32;
        let curve = CompressorCurve::from_index(
            self.shared.params[P_CURVE].load(Ordering::Relaxed).round().clamp(0.0, 2.0) as u32,
        );
        let range_target = self.shared.params[P_RANGE].load(Ordering::Relaxed).max(0.0);
        let hold_target = self.shared.params[P_HOLD].load(Ordering::Relaxed).max(0.0);
        let sc_hpf_idx = self.shared.params[P_SC_HPF]
            .load(Ordering::Relaxed)
            .round() as u32;
        let sc_hpf_freq = sc_hpf_hz(sc_hpf_idx);
        let auto_rel = self.shared.params[P_AUTO_REL].load(Ordering::Relaxed) > 0.5;
        let ms_mode = self.shared.params[P_MS_MODE].load(Ordering::Relaxed) >= 0.5;
        let dual = self.dual_block();
        self.blk_gr_rms = 0.0;
        self.blk_gr_peak = 0.0;

        // Lookahead drives CLAP latency directly. Hosts only re-do PDC
        // when this value changes; a Relaxed store on a clamped knob is
        // cheap enough to send every block.
        let look_samples_target = sr * 0.001 * lookahead_ms_target.min(MAX_LOOKAHEAD_MS);
        // An oversampled ceiling delays the wet path by the halfband pair;
        // the dry tap is read that much further back to match (see the
        // ceiling block), so the plugin as a whole is that much later too.
        let os_latency = if ceiling_target < -0.05 {
            OVERSAMPLE_LATENCY[os_mode as usize]
        } else {
            0
        };
        self.shared
            .latency_samples
            .store(look_samples_target as u32 + os_latency as u32, Ordering::Relaxed);

        let mut max_gr_db: f32 = 0.0;

        // ---- Snapshot the sidechain port (index 1) before touching main ----
        // AI-NOTE: keying, latch persistence and release-on-silent-key are
        // pinned by tests/sidechain_e2e.rs (two-port clack-host e2e).
        let sc_present = self.sc.capture(&mut audio, 1)?;

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
                    lookahead_ms_target, ceiling_target, link_target,
                    os_mode, curve, range_target, hold_target,
                    sc_hpf_freq, sc_present, auto_rel,
                    ms_mode, dual,
                    &mut max_gr_db,
                );
            } else {
                // Mono: run the one true engine with R = L. The write-back
                // uses only L; the scratch R keeps the block's in/out split.
                // Always Some — set at activate, put back below every call.
                if let (Some(mut r_in), Some(mut r_out)) =
                    (self.mono_r_in.take(), self.mono_r_out.take())
                {
                let n = l_read.len().min(r_in.len());
                r_in[..n].copy_from_slice(&l_read[..n]);
                process_stereo_block(
                    self, &l_read[..n], &mut l_write[..n], &r_in[..n], &mut r_out[..n], sr,
                    threshold_target, ratio_target, attack_target, release_target,
                    knee_target, makeup_target, mix_target,
                    lookahead_ms_target, ceiling_target, link_target,
                    os_mode, curve, range_target, hold_target,
                    sc_hpf_freq, sc_present, auto_rel,
                    ms_mode, dual,
                    &mut max_gr_db,
                );
                self.mono_r_in = Some(r_in);
                self.mono_r_out = Some(r_out);
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
        self.shared.gr_rms_db.store(self.blk_gr_rms, Ordering::Relaxed);
        self.shared.gr_peak_db.store(self.blk_gr_peak, Ordering::Relaxed);

        Ok(ProcessStatus::Continue)
    }
}

impl PluginAudioProcessor<'_> {
    /// Read the Dual-detector and bus-key params, and the key level once per
    /// block (the bus updates ~20×/s and lags 50–100 ms, so per-sample reads
    /// would buy nothing).
    fn dual_block(&self) -> DualBlock {
        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let key_db = detector::key_level_db(&self.shared.scope, load(P_KEY_LO), load(P_KEY_HI));
        self.shared.key_level_db.store(key_db, Ordering::Relaxed);
        let dual = load(P_DETECTOR) >= 0.5;
        // The key is a band RMS. Classic's detector is peak-ish, so the key is
        // lifted by a sine's crest to read on its scale; Dual's RMS stage
        // squares it and wants the RMS as is (+3 dB there ducked early).
        let crest = if dual { 0.0 } else { detector::KEY_CREST_DB };
        DualBlock {
            dual,
            crest_db: load(P_PEAK_CREST).max(0.0),
            release_peak_ms: load(P_RELEASE_PEAK).max(1.0),
            release_rms_ms: load(P_RELEASE_RMS).max(1.0),
            key_amt: load(P_KEY).clamp(0.0, 1.0),
            key_lin: 10f32.powf((key_db + crest) / 20.0),
        }
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
    lookahead_ms_target: f32, ceiling_target: f32, link_target: f32,
    os_mode: u32, curve: CompressorCurve,
    range_target: f32, hold_target: f32,
    sc_hpf_freq: Option<f32>, sc_present: bool, auto_rel: bool,
    ms_mode: bool,
    d: DualBlock,
    max_gr_db: &mut f32,
) {
    let n = l_read.len().min(r_read.len());
    let look_max_samples = sr * 0.001 * MAX_LOOKAHEAD_MS;
    for i in 0..n {
        // Capture the raw L/R for scope + ceiling reference. If M/S
        // mode is on, the "L"/"R" we feed through the comp inner loop
        // are actually Mid/Side encodings — the loop math doesn't know
        // or care about the difference. We decode back to L/R at the
        // bottom before writing to the output buffers.
        let raw_l = l_read[i];
        let raw_r = r_read[i];
        let (dry_l, dry_r) = if ms_mode {
            ((raw_l + raw_r) * 0.5, (raw_l - raw_r) * 0.5)
        } else {
            (raw_l, raw_r)
        };

        let threshold = p.smooth_threshold.step(threshold_target, sr);
        let ratio = p.smooth_ratio.step(ratio_target, sr);
        let attack = p.smooth_attack.step(attack_target, sr);
        let mut release = p.smooth_release.step(release_target, sr);
        let knee = p.smooth_knee.step(knee_target, sr);
        let makeup = p.smooth_makeup.step(makeup_target, sr);
        let mix = p.smooth_mix.step(mix_target, sr);
        let lookahead_ms = p.smooth_lookahead.step(lookahead_ms_target, sr).max(0.0);
        let ceiling_db = p.smooth_ceiling.step(ceiling_target, sr);
        let link = p.smooth_link.step(link_target, sr).clamp(0.0, 1.0);
        let look_samples = (sr * 0.001 * lookahead_ms).min(look_max_samples);
        p.lookahead_samples = look_samples;

        let mut release_rms = d.release_rms_ms;
        if auto_rel {
            let stretch = 1.0 + 3.0 * p.sustained_gr_env.min(1.0);
            release *= stretch;
            release_rms *= stretch;
        }

        let sc = detector_input(p, i, dry_l, dry_r, sc_present, sc_hpf_freq, link, sr);
        let sc = detector::blend_key(sc, d.key_lin, d.key_amt);
        let cfg = detector::DetCfg {
            threshold, ratio, knee, curve, attack_ms: attack, release_ms: release,
            dual: d.dual, crest_db: d.crest_db,
            release_peak_ms: d.release_peak_ms, release_rms_ms: release_rms,
        };
        let det = p.detector.process(sc, &cfg, sr);
        p.blk_gr_rms = p.blk_gr_rms.min(det.rms_gr_db);
        p.blk_gr_peak = p.blk_gr_peak.min(det.peak_gr_db);
        let mut gr_db = det.gr_db;

        // Range — hard floor on GR. Single source of the rule (incl. the
        // 0.05 dB deadband): synth_core::dsp_blocks::apply_range.
        gr_db = superduper_synth_core::dsp_blocks::apply_range(gr_db, range_target);

        gr_db = apply_hold(p, gr_db, hold_target, sr);

        if gr_db < *max_gr_db { *max_gr_db = gr_db; }

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
        let delayed_l = p.look_l.read_lagrange3(look_samples);
        let delayed_r = p.look_r.read_lagrange3(look_samples);

        let mut wet_l = delayed_l * gain_lin;
        let mut wet_r = delayed_r * gain_lin;

        // Soft tanh ceiling — optionally evaluated at 2× or 4× the host
        // rate to push the tanh's harmonics above Nyquist where the
        // halfband decimator buries them. Off mode = native rate, no
        // CPU overhead beyond the comparison. ZLCompressor's Oversample
        // setting drives the same thing.
        let (mut dry_mix_l, mut dry_mix_r) = (delayed_l, delayed_r);
        if ceiling_db < -0.05 {
            let ceil_lin = 10f32.powf(ceiling_db / 20.0).max(1e-4);
            let clip = |s: f32| ceil_lin * (s / ceil_lin).tanh();
            wet_l = oversample_apply(wet_l, os_mode, &mut p.os1_l, &mut p.os2_l, clip);
            wet_r = oversample_apply(wet_r, os_mode, &mut p.os1_r, &mut p.os2_r, clip);
            // Oversampling delays wet by OVERSAMPLE_LATENCY; take the dry
            // for Mix from that much further back or the blend combs.
            let os_lat = OVERSAMPLE_LATENCY[(os_mode as usize).min(2)] as f32;
            if os_lat > 0.0 {
                dry_mix_l = p.look_l.read_lagrange3(look_samples + os_lat);
                dry_mix_r = p.look_r.read_lagrange3(look_samples + os_lat);
            }
        }

        let out_l = dry_mix_l * (1.0 - mix) + wet_l * mix;
        let out_r = dry_mix_r * (1.0 - mix) + wet_r * mix;
        // M/S decode back to L/R for the host. L = M+S, R = M-S.
        let (final_l, final_r) = if ms_mode {
            (out_l + out_r, out_l - out_r)
        } else {
            (out_l, out_r)
        };
        l_write[i] = final_l;
        r_write[i] = final_r;

        // Scope frame — peak of raw input vs final output (what the
        // host actually sees), not the M/S intermediate.
        let in_peak = raw_l.abs().max(raw_r.abs());
        let out_peak = final_l.abs().max(final_r.abs());
        push_scope(p, in_peak, out_peak, gr_db);
        p.shared.scope.push(0.5 * (final_l + final_r));
    }
}

/// Hold stage on the per-sample GR. Returns the GR to apply.
#[inline]
fn apply_hold(p: &mut PluginAudioProcessor<'_>, mut gr_db: f32, hold_target: f32, sr: f32) -> f32 {
    // Hold — when the static curve has stopped asking for more GR
    // (compressing → released stage), keep the previous GR latched
    // for hold_target ms. The detector's own release still ticks down
    // behind the scenes; we just delay the moment the GR follows it.
    // Active hold extends only when the GR is currently below the
    // latched value (i.e. the detector is recovering, not still
    // attacking deeper).
    if hold_target > 0.05 {
        let hold_samples = hold_target * 0.001 * sr;
        if gr_db < p.hold_gr_db - 0.05 {
            // Going deeper — restart hold window with the new lower
            // GR. (gr_db is negative; "lower" means more reduction.)
            p.hold_gr_db = gr_db;
            p.hold_remaining_samples = hold_samples;
        } else if gr_db > p.hold_gr_db + 0.05 && p.hold_remaining_samples > 0.0 {
            // Recovery requested but hold timer still running — latch
            // GR at the held value.
            gr_db = p.hold_gr_db;
            p.hold_remaining_samples -= 1.0;
        } else {
            // Either GR matches the held value (stable compression) or
            // the hold timer expired. Either way the latch follows the
            // detector again; clear the countdown so it doesn't tick
            // past zero pointlessly.
            p.hold_gr_db = gr_db;
            p.hold_remaining_samples = 0.0;
        }
    }
    gr_db
}

/// The detector's input for sample `i`: sidechain port (when routed) or the
/// dry signal, SC HPF on the signed audio, then rectified and stereo-linked.
#[allow(clippy::too_many_arguments)]
#[inline]
fn detector_input(
    p: &mut PluginAudioProcessor<'_>, i: usize, dry_l: f32, dry_r: f32,
    sc_present: bool, sc_hpf_freq: Option<f32>, link: f32, sr: f32,
) -> f32 {
    let (mut key_l, mut key_r) = if sc_present {
        (
            p.sc.l.get(i).copied().unwrap_or(0.0),
            p.sc.r.get(i).copied().unwrap_or(0.0),
        )
    } else {
        (dry_l, dry_r)
    };
    // SC HPF on raw signed audio — see sc_hpf_repro.rs for the
    // rationale (must precede rectification or it crushes detection).
    if let Some(hp_hz) = sc_hpf_freq {
        let coef = (-core::f32::consts::TAU * hp_hz / sr).exp();
        p.sc_hpf_state_l = key_l * (1.0 - coef) + p.sc_hpf_state_l * coef;
        key_l -= p.sc_hpf_state_l;
        p.sc_hpf_state_r = key_r * (1.0 - coef) + p.sc_hpf_state_r * coef;
        key_r -= p.sc_hpf_state_r;
    }
    // Stereo coupling — `link=1` follows the louder channel (fully
    // linked), `link=0` would feed each channel its own detector
    // value (independent). With a single shared detector here we
    // blend max(|L|,|R|) toward mean(|L|,|R|) as link decreases.
    // Fully independent per-channel detectors would need two
    // EnvelopeDetector instances — kept simple for now.
    let abs_l = key_l.abs();
    let abs_r = key_r.abs();
    let max_lr = abs_l.max(abs_r);
    let mean_lr = (abs_l + abs_r) * 0.5;
    max_lr * link + mean_lr * (1.0 - link)
}

/// Decimating push to the scope ring buffer. Aggregates SCOPE_HOP samples
/// (taking the peak |x| in / out and the most-negative GR in dB) before
/// emitting one frame. dB conversions happen here so the GUI just reads
/// floats out of the ring and renders without per-frame log10 calls.
#[inline]
fn push_scope(p: &mut PluginAudioProcessor<'_>, in_lin: f32, out_lin: f32, gr_db: f32) {
    if in_lin > p.scope_acc_in { p.scope_acc_in = in_lin; }
    if out_lin > p.scope_acc_out { p.scope_acc_out = out_lin; }
    if gr_db < p.scope_acc_gr { p.scope_acc_gr = gr_db; }
    p.scope_acc_count += 1;
    if p.scope_acc_count >= SCOPE_HOP {
        let in_db = 20.0 * p.scope_acc_in.max(1e-7).log10();
        let out_db = 20.0 * p.scope_acc_out.max(1e-7).log10();
        p.shared.meter.push(in_db, out_db, p.scope_acc_gr);
        p.scope_acc_in = 0.0;
        p.scope_acc_out = 0.0;
        p.scope_acc_gr = 0.0;
        p.scope_acc_count = 0;
    }
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
                // No in-place: the host must hand us separate input and output
                // buffers. With in-place allowed, split_io had to return a
                // &[f32] and a &mut [f32] over the SAME memory to keep the
                // "read x[i], write y[i]" style working — two noalias slices
                // aliasing each other, which is undefined behaviour whatever
                // the access order. The host's own copy costs the same as the
                // scratch buffer we would otherwise keep per plugin.
                in_place_pair: None,
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
        use core::fmt::Write;
        let pid = id.get() as usize;
        if pid == P_MS_MODE {
            return write!(w, "{}", if v >= 0.5 { "M/S" } else { "L/R" });
        }
        if pid == P_DETECTOR {
            return write!(w, "{}", if v >= 0.5 { "Dual" } else { "Classic" });
        }
        if pid == P_AUTO_REL {
            return write!(w, "{}", if v >= 0.5 { "On" } else { "Off" });
        }
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
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::track_info::PluginTrackInfo>()
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
        .with_version(version_string!("0.2"))
        .with_description("Soft-knee feed-forward compressor with lookahead and sidechain HPF")
        .with_features([AUDIO_EFFECT, STEREO, COMPRESSOR])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Compressor — build {} ({})", build_num!(), build_date!());
        let shared = PluginShared::new();
        superduper_synth_core::gui::spawn_bus_publisher_mcp("SuperDuper Compressor", &shared.inner, |s| &s.scope, Some(superduper_synth_core::gui::ParamsAccess { defs: PARAMS, values: |s| &s.params, dirty: Some(|s| &s.dirty_params) }), Some(mail::handle));
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

clack_export_entry!(SinglePluginEntry<SuperDuperCompressor>);

impl clack_extensions::track_info::PluginTrackInfoImpl for PluginMainThread<'_> {
    fn changed(&self) {
        self.track_watch.refresh();
    }
}
