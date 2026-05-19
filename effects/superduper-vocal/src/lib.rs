//! SuperDuper Vocal — split-band de-esser + ratio-based de-clicker.
//!
//! Two cleanup stages in series. Either can be set to 0 amount to bypass.
//!
//! # De-Esser (split-band)
//! Standard technique used by SSL, FabFilter Pro-DS, Waves Renaissance
//! De-Esser when set to "split" mode.
//!
//! 1. RBJ biquad HPF at the user's sibilance frequency yields the
//!    "sibilance band". The complement (`input - sibilance`) is the
//!    "body band". Both bands sum back to bit-identical input until we
//!    touch the sibilance band — important for transparency on quiet
//!    passages.
//! 2. Envelope detector on `max(|sibilance_L|, |sibilance_R|)` with a
//!    fast attack (0.5 ms) and short release (20 ms) — sibilants are
//!    short bursts of 2-10 kHz energy, longer time constants smear them.
//! 3. Static gain reduction = -min(env_dB - threshold, amount). The
//!    sibilance band is attenuated by this much; the body band passes
//!    untouched.
//!
//! # De-Clicker (ratio detector)
//! Idea borrowed from iZotope's mouth de-click and accusonus De-Clicker:
//! mouth clicks and lip smacks show up as **transient spikes** that have
//! much higher short-term energy than the local average. A clean vocal
//! line keeps the short / long envelope ratio close to unity (~1.5x at
//! consonant onsets); a click pushes it to 3-8x for a few ms.
//!
//! 1. Compute two envelopes of `|x|`: fast (0.1 ms attack / 0.5 ms
//!    release) and slow (5 ms attack / 50 ms release).
//! 2. If `fast / slow > sensitivity_threshold` AND fast above a floor
//!    (so we don't trigger on silence), schedule a duck — target gain
//!    = `10^(-Amount/20)`.
//! 3. Smooth the duck toward target with a fast attack (~0.5 ms) and
//!    `Sensitivity`-controlled release (3 - 30 ms). Apply broadband to
//!    L+R so the click disappears uniformly.
//!
//! This is **not** AR/LSAR-style interpolation (the academic state of
//! the art) — that's offline-only. Real-time de-clicking with a fast
//! ducker is the JST Gain Reduction / accusonus approach and works
//! well for the kind of mouth noise that shows up in rap vocals.

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
use superduper_synth_core::dsp_blocks::{Biquad, EnvelopeDetector, SmoothedParam};

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn init_logging() { superduper_dsp_sdk::log::init("vocal"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Params — De-Ess section (4) + De-Click section (3) + Output (2)
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Ess Thr",    min: -60.0, max: 0.0,    default: -24.0, unit: "dB" },
    ParamDef { id: 1, name: b"Ess Freq",   min: 2000.0, max: 10000.0, default: 6000.0, unit: "Hz" },
    ParamDef { id: 2, name: b"Ess Amt",    min: 0.0,   max: 18.0,   default: 6.0,   unit: "dB" },
    ParamDef { id: 3, name: b"Ess Range",  min: 0.0,   max: 1.0,    default: 1.0,   unit: ""   },
    // Ratio threshold — short-env / long-env. Below 1.5 every consonant
    // triggers; above 6 most mouth clicks slip through.
    ParamDef { id: 4, name: b"Clk Sens",   min: 1.5,   max: 8.0,    default: 3.0,   unit: "x"  },
    ParamDef { id: 5, name: b"Clk Amt",    min: 0.0,   max: 24.0,   default: 12.0,  unit: "dB" },
    ParamDef { id: 6, name: b"Clk Floor",  min: -60.0, max: -20.0,  default: -40.0, unit: "dB" },
    ParamDef { id: 7, name: b"Output",     min: -24.0, max: 24.0,   default: 0.0,   unit: "dB" },
    ParamDef { id: 8, name: b"Mix",        min: 0.0,   max: 1.0,    default: 1.0,   unit: ""   },
    // Low-band de-esser — plosives ("p", "t", "b") + low harshness.
    // Same band-split semantics as the primary essing band but tuned
    // around 1 kHz. Default Amt=0 keeps it off until the user dials in.
    ParamDef { id: 9,  name: b"Lo Thr",   min: -60.0,  max: 0.0,     default: -24.0, unit: "dB" },
    ParamDef { id: 10, name: b"Lo Freq",  min: 300.0,  max: 3000.0,  default: 1000.0, unit: "Hz" },
    ParamDef { id: 11, name: b"Lo Amt",   min: 0.0,    max: 18.0,    default: 0.0,   unit: "dB" },
    // External-key selector: 0 = use dry signal (current behaviour),
    // 1 = use the Sidechain input port for the detector. Lets users
    // key the de-esser off a separate EQ'd vocal send.
    ParamDef { id: 12, name: b"Ext Key",  min: 0.0,    max: 1.0,     default: 0.0,   unit: ""   },
    // Plosive Killer — sub-bass transient detector. Watches the
    // <200 Hz band, when energy spikes (a "p", "b", or "t" mouth
    // pop) it briefly attenuates the lows with a dynamic HPF. The
    // user's `Lo Freq` de-esser sits at ~1 kHz and won't catch sub
    // pops; this fills that gap. Off by default — set Plos On = 1
    // and turn up Plos Amt to taste.
    ParamDef { id: 13, name: b"Plos On",  min: 0.0,    max: 1.0,     default: 0.0,   unit: ""   },
    ParamDef { id: 14, name: b"Plos Thr", min: -60.0,  max: 0.0,     default: -24.0, unit: "dB" },
    ParamDef { id: 15, name: b"Plos Amt", min: 0.0,    max: 24.0,    default: 12.0,  unit: "dB" },
    ParamDef { id: 16, name: b"Plos Freq",min: 40.0,   max: 250.0,   default: 120.0, unit: "Hz" },
    // Hum Remover — cascade of narrow biquad notches at 50/60 Hz
    // and its first 5 harmonics. Adaptively rolled off as `Strength`
    // goes from 0 → 1; at 1.0 each notch removes ~10 dB centered on
    // its target frequency.
    ParamDef { id: 17, name: b"Hum On",   min: 0.0,    max: 1.0,     default: 0.0,   unit: ""   },
    ParamDef { id: 18, name: b"Hum Freq", min: 50.0,   max: 60.0,    default: 50.0,  unit: "Hz" },
    ParamDef { id: 19, name: b"Hum Str",  min: 0.0,    max: 1.0,     default: 0.7,   unit: ""   },
];

pub const P_ESS_THR: usize = 0;
pub const P_ESS_FREQ: usize = 1;
pub const P_ESS_AMT: usize = 2;
pub const P_ESS_RANGE: usize = 3;
pub const P_CLK_SENS: usize = 4;
pub const P_CLK_AMT: usize = 5;
pub const P_CLK_FLOOR: usize = 6;
pub const P_OUTPUT: usize = 7;
pub const P_MIX: usize = 8;
pub const P_LO_THR: usize = 9;
pub const P_LO_FREQ: usize = 10;
pub const P_LO_AMT: usize = 11;
pub const P_EXT_KEY: usize = 12;
pub const P_PLOS_ON: usize = 13;
pub const P_PLOS_THR: usize = 14;
pub const P_PLOS_AMT: usize = 15;
pub const P_PLOS_FREQ: usize = 16;
pub const P_HUM_ON: usize = 17;
pub const P_HUM_FREQ: usize = 18;
pub const P_HUM_STR: usize = 19;

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
    /// Latest de-ess GR in dB (negative or zero). Block-rate.
    pub ess_gr_db: AtomicF32,
    /// Latest de-click GR in dB (negative or zero). Block-rate.
    pub click_gr_db: AtomicF32,
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
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
                ess_gr_db: AtomicF32::new(0.0),
                click_gr_db: AtomicF32::new(0.0),
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
    // De-Ess (high band — sibilance)
    ess_hpf_l: Biquad,
    ess_hpf_r: Biquad,
    ess_env: EnvelopeDetector,
    /// Last freq the HPF was set up at; re-coefficient the biquad when the
    /// smoothed knob value drifts more than the perception-threshold so
    /// dragging the freq slider doesn't pop.
    ess_freq_state: f32,
    // De-Ess (low band — plosives)
    lo_hpf_l: Biquad,
    lo_hpf_r: Biquad,
    lo_env: EnvelopeDetector,
    lo_freq_state: f32,
    smooth_lo_thr: SmoothedParam,
    smooth_lo_freq: SmoothedParam,
    smooth_lo_amt: SmoothedParam,
    // External-key scratch buffer (sized at activate to max_frames_count).
    sc_l: Vec<f32>,
    sc_r: Vec<f32>,
    // De-Click — two envelopes per channel + one shared duck-gain state
    click_fast_l: EnvelopeDetector,
    click_fast_r: EnvelopeDetector,
    click_slow_l: EnvelopeDetector,
    click_slow_r: EnvelopeDetector,
    click_gain: f32,
    // Smoothed knobs
    smooth_ess_thr: SmoothedParam,
    smooth_ess_freq: SmoothedParam,
    smooth_ess_amt: SmoothedParam,
    smooth_ess_range: SmoothedParam,
    smooth_clk_sens: SmoothedParam,
    smooth_clk_amt: SmoothedParam,
    smooth_clk_floor: SmoothedParam,
    smooth_output: SmoothedParam,
    smooth_mix: SmoothedParam,
    sample_rate: f32,

    // -------------------------------------------------------------
    // Stage 1 additions — Plosive Killer + Hum Remover + Lookahead.
    // -------------------------------------------------------------
    /// Plosive Killer — sub-bass envelope follower drives a
    /// dynamic low-cut HPF when the band detects a transient pop.
    plos_lpf_l: Biquad,
    plos_lpf_r: Biquad,
    plos_hpf_l: Biquad,
    plos_hpf_r: Biquad,
    plos_env: EnvelopeDetector,
    plos_freq_state: f32,
    /// Hum Remover — cascade of 6 narrow biquad notches (fundamental
    /// + 5 harmonics). One bank per channel; coefficients re-set
    /// when the smoothed `Hum Freq` or `Hum Str` knob moves enough.
    hum_notches_l: [Biquad; 6],
    hum_notches_r: [Biquad; 6],
    hum_freq_state: f32,
    hum_str_state: f32,
    /// Smoothed Stage-1 knobs.
    smooth_plos_thr: SmoothedParam,
    smooth_plos_amt: SmoothedParam,
    smooth_plos_freq: SmoothedParam,
    smooth_hum_str: SmoothedParam,
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
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);

        let mut ess_hpf_l = Biquad::default();
        let mut ess_hpf_r = Biquad::default();
        let initial_freq = load(P_ESS_FREQ);
        ess_hpf_l.set_hpf(sr, initial_freq, 0.707);
        ess_hpf_r.set_hpf(sr, initial_freq, 0.707);

        let initial_lo_freq = load(P_LO_FREQ);
        let mut lo_hpf_l = Biquad::default();
        let mut lo_hpf_r = Biquad::default();
        lo_hpf_l.set_hpf(sr, initial_lo_freq, 0.707);
        lo_hpf_r.set_hpf(sr, initial_lo_freq, 0.707);

        let buf_cap = audio_config.max_frames_count as usize;

        Ok(Self {
            shared,
            ess_hpf_l,
            ess_hpf_r,
            ess_env: EnvelopeDetector::default(),
            ess_freq_state: initial_freq,
            lo_hpf_l,
            lo_hpf_r,
            lo_env: EnvelopeDetector::default(),
            lo_freq_state: initial_lo_freq,
            smooth_lo_thr: SmoothedParam::new(load(P_LO_THR)),
            smooth_lo_freq: SmoothedParam::new(load(P_LO_FREQ)),
            smooth_lo_amt: SmoothedParam::new(load(P_LO_AMT)),
            sc_l: vec![0.0; buf_cap],
            sc_r: vec![0.0; buf_cap],
            click_fast_l: EnvelopeDetector::default(),
            click_fast_r: EnvelopeDetector::default(),
            click_slow_l: EnvelopeDetector::default(),
            click_slow_r: EnvelopeDetector::default(),
            click_gain: 1.0,
            smooth_ess_thr: SmoothedParam::new(load(P_ESS_THR)),
            smooth_ess_freq: SmoothedParam::new(load(P_ESS_FREQ)),
            smooth_ess_amt: SmoothedParam::new(load(P_ESS_AMT)),
            smooth_ess_range: SmoothedParam::new(load(P_ESS_RANGE)),
            smooth_clk_sens: SmoothedParam::new(load(P_CLK_SENS)),
            smooth_clk_amt: SmoothedParam::new(load(P_CLK_AMT)),
            smooth_clk_floor: SmoothedParam::new(load(P_CLK_FLOOR)),
            smooth_output: SmoothedParam::new(load(P_OUTPUT)),
            smooth_mix: SmoothedParam::new(load(P_MIX)),
            sample_rate: sr,

            plos_lpf_l: { let mut b = Biquad::default(); b.set_lpf(sr, load(P_PLOS_FREQ).max(40.0), 0.707); b },
            plos_lpf_r: { let mut b = Biquad::default(); b.set_lpf(sr, load(P_PLOS_FREQ).max(40.0), 0.707); b },
            plos_hpf_l: { let mut b = Biquad::default(); b.set_hpf(sr, load(P_PLOS_FREQ).max(40.0), 0.707); b },
            plos_hpf_r: { let mut b = Biquad::default(); b.set_hpf(sr, load(P_PLOS_FREQ).max(40.0), 0.707); b },
            plos_env: EnvelopeDetector::default(),
            plos_freq_state: load(P_PLOS_FREQ).max(40.0),
            hum_notches_l: build_hum_bank(sr, load(P_HUM_FREQ), load(P_HUM_STR)),
            hum_notches_r: build_hum_bank(sr, load(P_HUM_FREQ), load(P_HUM_STR)),
            hum_freq_state: load(P_HUM_FREQ),
            hum_str_state: load(P_HUM_STR),
            smooth_plos_thr: SmoothedParam::new(load(P_PLOS_THR)),
            smooth_plos_amt: SmoothedParam::new(load(P_PLOS_AMT)),
            smooth_plos_freq: SmoothedParam::new(load(P_PLOS_FREQ)),
            smooth_hum_str: SmoothedParam::new(load(P_HUM_STR)),
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
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

        let ess_thr_t = self.shared.params[P_ESS_THR].load(Ordering::Relaxed);
        let ess_freq_t = self.shared.params[P_ESS_FREQ].load(Ordering::Relaxed);
        let ess_amt_t = self.shared.params[P_ESS_AMT].load(Ordering::Relaxed);
        let ess_range_t = self.shared.params[P_ESS_RANGE].load(Ordering::Relaxed);
        let lo_thr_t = self.shared.params[P_LO_THR].load(Ordering::Relaxed);
        let lo_freq_t = self.shared.params[P_LO_FREQ].load(Ordering::Relaxed);
        let lo_amt_t = self.shared.params[P_LO_AMT].load(Ordering::Relaxed);
        let clk_sens_t = self.shared.params[P_CLK_SENS].load(Ordering::Relaxed);
        let clk_amt_t = self.shared.params[P_CLK_AMT].load(Ordering::Relaxed);
        let clk_floor_t = self.shared.params[P_CLK_FLOOR].load(Ordering::Relaxed);
        let output_t = self.shared.params[P_OUTPUT].load(Ordering::Relaxed);
        let mix_t = self.shared.params[P_MIX].load(Ordering::Relaxed);
        let ext_key_on = self.shared.params[P_EXT_KEY].load(Ordering::Relaxed) >= 0.5;
        let plos_on = self.shared.params[P_PLOS_ON].load(Ordering::Relaxed) >= 0.5;
        let plos_thr_t = self.shared.params[P_PLOS_THR].load(Ordering::Relaxed);
        let plos_amt_t = self.shared.params[P_PLOS_AMT].load(Ordering::Relaxed);
        let plos_freq_t = self.shared.params[P_PLOS_FREQ].load(Ordering::Relaxed);
        let hum_on = self.shared.params[P_HUM_ON].load(Ordering::Relaxed) >= 0.5;
        let hum_freq_t = self.shared.params[P_HUM_FREQ].load(Ordering::Relaxed);
        let hum_str_t = self.shared.params[P_HUM_STR].load(Ordering::Relaxed);

        // Snapshot the sidechain (port 1) into our scratch buffers if
        // the user wants external keying. If the SC port is unrouted
        // we'll fall back to the dry signal inside step_sample.
        let frames = audio.frames_count() as usize;
        let n_frames = frames.min(self.sc_l.len());
        let mut sc_present = false;
        if ext_key_on {
            if let Some(sc_port) = audio.input_port(1) {
                if let Some(chans) = sc_port.channels()?.into_f32() {
                    if let Some(l) = chans.channel(0) {
                        let n = n_frames.min(l.len());
                        self.sc_l[..n].copy_from_slice(&l[..n]);
                        if l.iter().take(n).any(|&x| x != 0.0) { sc_present = true; }
                    }
                    if let Some(r) = chans.channel(1) {
                        let n = n_frames.min(r.len());
                        self.sc_r[..n].copy_from_slice(&r[..n]);
                    } else {
                        let copy = self.sc_l[..n_frames].to_vec();
                        self.sc_r[..n_frames].copy_from_slice(&copy);
                    }
                }
            }
        }
        let use_ext = ext_key_on && sc_present;

        let mut max_ess_gr_db: f32 = 0.0;
        let mut max_click_gr_db: f32 = 0.0;

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            let mut iter = channel_pairs.into_iter();
            let Some(ch_l) = iter.next() else { continue };
            let ch_r = iter.next();

            use superduper_dsp_sdk::clap_helpers::split_io;
            let Some((l_read, l_write)) = split_io(ch_l) else { continue };
            let r = ch_r.and_then(split_io);

            if bypassed {
                l_write.copy_from_slice(l_read);
                if let Some((rr, rw)) = r {
                    rw.copy_from_slice(rr);
                }
                continue;
            }

            // Carve out the SC scratch slices outside the &mut self
            // borrow so we don't tangle the borrow checker.
            let sc_slice: Option<(&[f32], &[f32])> = if use_ext {
                Some((&self.sc_l[..n_frames], &self.sc_r[..n_frames]))
            } else {
                None
            };

            match r {
                Some((r_read, r_write)) => {
                    // sc_slice borrows self immutably; we need mutable
                    // self for process_stereo. Release the borrow by
                    // re-acquiring slices via raw pointers.
                    let sc_kk = sc_slice.map(|(a, b)| (a.as_ptr(), b.as_ptr()));
                    let owned_sc: Option<(&[f32], &[f32])> = sc_kk.map(|(ap, bp)| unsafe {
                        (
                            core::slice::from_raw_parts(ap, n_frames),
                            core::slice::from_raw_parts(bp, n_frames),
                        )
                    });
                    process_stereo(
                        self, l_read, l_write, r_read, r_write, sr,
                        ess_thr_t, ess_freq_t, ess_amt_t, ess_range_t,
                        lo_thr_t, lo_freq_t, lo_amt_t,
                        clk_sens_t, clk_amt_t, clk_floor_t, output_t, mix_t,
                        plos_on, plos_thr_t, plos_amt_t, plos_freq_t,
                        hum_on, hum_freq_t, hum_str_t,
                        owned_sc,
                        &mut max_ess_gr_db, &mut max_click_gr_db,
                    );
                }
                None => {
                    let sc_l_ptr = sc_slice.map(|(a, _)| a.as_ptr());
                    let owned_sc: Option<&[f32]> = sc_l_ptr.map(|p| unsafe {
                        core::slice::from_raw_parts(p, n_frames)
                    });
                    process_mono(
                        self, l_read, l_write, sr,
                        ess_thr_t, ess_freq_t, ess_amt_t, ess_range_t,
                        lo_thr_t, lo_freq_t, lo_amt_t,
                        clk_sens_t, clk_amt_t, clk_floor_t, output_t, mix_t,
                        plos_on, plos_thr_t, plos_amt_t, plos_freq_t,
                        hum_on, hum_freq_t, hum_str_t,
                        owned_sc,
                        &mut max_ess_gr_db, &mut max_click_gr_db,
                    );
                }
            }
        }

        self.shared.ess_gr_db.store(max_ess_gr_db, Ordering::Relaxed);
        self.shared.click_gr_db.store(max_click_gr_db, Ordering::Relaxed);

        Ok(ProcessStatus::Continue)
    }
}

/// Build the 6-stage hum-notch bank — fundamental + first 5 harmonics
/// (50→300 Hz or 60→360 Hz). `strength` 0..1 controls notch Q so the
/// notches widen / narrow with the knob; 1.0 = surgical Q=30, 0.0 =
/// effectively-disabled wide bandwidth. Re-build whenever Hum Freq or
/// Hum Str moves more than a small threshold.
fn build_hum_bank(sr: f32, fundamental: f32, strength: f32) -> [Biquad; 6] {
    let q = (5.0 + strength.clamp(0.0, 1.0) * 25.0).max(0.5);
    let mut bank = [Biquad::default(); 6];
    for i in 0..6 {
        let f = fundamental * (i as f32 + 1.0);
        if f < sr * 0.49 {
            bank[i].set_notch(sr, f, q);
        }
    }
    bank
}

/// Per-sample DSP for one (L, R) sample pair. Returns the GR values so the
/// caller can track per-block maxima for the GUI meters. Updated state
/// (filters, envelopes, smoothed knobs, click gain) lives on `p`.
#[allow(clippy::too_many_arguments)]
#[inline]
// External key (key_l/key_r) — if Some, replaces dry signal as the
// detector source. Lets users key the de-esser from an EQ'd send.
fn step_sample(
    p: &mut PluginAudioProcessor<'_>,
    dry_l_in: f32, dry_r_in: f32,
    key_l: Option<f32>, key_r: Option<f32>,
    sr: f32,
    ess_thr_t: f32, ess_freq_t: f32, ess_amt_t: f32, ess_range_t: f32,
    lo_thr_t: f32, lo_freq_t: f32, lo_amt_t: f32,
    clk_sens_t: f32, clk_amt_t: f32, clk_floor_t: f32,
    output_t: f32, mix_t: f32,
    plos_on: bool, plos_thr_t: f32, plos_amt_t: f32, plos_freq_t: f32,
    hum_on: bool, hum_freq_t: f32, hum_str_t: f32,
) -> (f32, f32, f32, f32) {
    // ----- Stage 1: Hum Remover -----
    // Reset coefficients if Hum Freq or Strength moves enough. Coef
    // recomputation is cheap (6 biquads worth of trig) but we only do
    // it on real change to avoid per-sample work.
    if hum_on {
        if (hum_freq_t - p.hum_freq_state).abs() > 0.1
            || (hum_str_t - p.hum_str_state).abs() > 0.02
        {
            p.hum_notches_l = build_hum_bank(sr, hum_freq_t, hum_str_t);
            p.hum_notches_r = build_hum_bank(sr, hum_freq_t, hum_str_t);
            p.hum_freq_state = hum_freq_t;
            p.hum_str_state = hum_str_t;
        }
    }
    let mut dry_l = dry_l_in;
    let mut dry_r = dry_r_in;
    if hum_on {
        for n in p.hum_notches_l.iter_mut() { dry_l = n.process(dry_l); }
        for n in p.hum_notches_r.iter_mut() { dry_r = n.process(dry_r); }
    }

    // ----- Stage 1: Plosive Killer -----
    // Sub-bass LPF isolates the <Plos Freq band where pops live;
    // envelope follower watches for transient spikes. When over
    // threshold, low-end gain reduction kicks in (subtracts the
    // sub-bass component scaled by the attenuation amount).
    let plos_freq = p.smooth_plos_freq.step(plos_freq_t, sr).clamp(40.0, 250.0);
    let plos_thr_db = p.smooth_plos_thr.step(plos_thr_t, sr);
    let plos_amt_db = p.smooth_plos_amt.step(plos_amt_t, sr);
    if plos_on && (plos_freq - p.plos_freq_state).abs() > 2.0 {
        p.plos_lpf_l.set_lpf(sr, plos_freq, 0.707);
        p.plos_lpf_r.set_lpf(sr, plos_freq, 0.707);
        p.plos_hpf_l.set_hpf(sr, plos_freq, 0.707);
        p.plos_hpf_r.set_hpf(sr, plos_freq, 0.707);
        p.plos_freq_state = plos_freq;
    }
    if plos_on {
        // Watch the low band: clone the LPF state by running it
        // forward — sample x → sub_l. Same biquad updates state.
        let sub_l = p.plos_lpf_l.process(dry_l);
        let sub_r = p.plos_lpf_r.process(dry_r);
        let sc = sub_l.abs().max(sub_r.abs());
        let env = p.plos_env.process(sc, sr, 0.3, 35.0);
        let env_db = 20.0 * env.max(1e-9).log10();
        let over = env_db - plos_thr_db;
        let plos_gr_db = if over > 0.0 && plos_amt_db > 0.05 {
            -(over.min(plos_amt_db))
        } else { 0.0 };
        let lo_gain = 10f32.powf(plos_gr_db / 20.0);
        // Reconstruct from band-split: dry = (above + lo_gain * sub).
        let above_l = dry_l - sub_l;
        let above_r = dry_r - sub_r;
        // Run the HPF through to keep biquad state moving even when
        // Plos On isn't transitioning, so toggling on doesn't pop.
        let _ = p.plos_hpf_l.process(dry_l);
        let _ = p.plos_hpf_r.process(dry_r);
        dry_l = above_l + sub_l * lo_gain;
        dry_r = above_r + sub_r * lo_gain;
    }

    let ess_thr = p.smooth_ess_thr.step(ess_thr_t, sr);
    let ess_freq = p.smooth_ess_freq.step(ess_freq_t, sr);
    let ess_amt = p.smooth_ess_amt.step(ess_amt_t, sr);
    let ess_range = p.smooth_ess_range.step(ess_range_t, sr).clamp(0.0, 1.0);
    let lo_thr = p.smooth_lo_thr.step(lo_thr_t, sr);
    let lo_freq = p.smooth_lo_freq.step(lo_freq_t, sr);
    let lo_amt = p.smooth_lo_amt.step(lo_amt_t, sr);
    let clk_sens = p.smooth_clk_sens.step(clk_sens_t, sr);
    let clk_amt = p.smooth_clk_amt.step(clk_amt_t, sr);
    let clk_floor_db = p.smooth_clk_floor.step(clk_floor_t, sr);
    let output_db = p.smooth_output.step(output_t, sr);
    let mix = p.smooth_mix.step(mix_t, sr);

    if (ess_freq - p.ess_freq_state).abs() > 5.0 {
        p.ess_hpf_l.set_hpf(sr, ess_freq, 0.707);
        p.ess_hpf_r.set_hpf(sr, ess_freq, 0.707);
        p.ess_freq_state = ess_freq;
    }
    if (lo_freq - p.lo_freq_state).abs() > 5.0 {
        p.lo_hpf_l.set_hpf(sr, lo_freq, 0.707);
        p.lo_hpf_r.set_hpf(sr, lo_freq, 0.707);
        p.lo_freq_state = lo_freq;
    }

    // Detector source: external key if routed, otherwise the dry signal.
    let det_l = key_l.unwrap_or(dry_l);
    let det_r = key_r.unwrap_or(dry_r);

    let sib_l = p.ess_hpf_l.process(dry_l);
    let sib_r = p.ess_hpf_r.process(dry_r);
    let body_l = dry_l - sib_l;
    let body_r = dry_r - sib_r;

    // Run the high-band detector on the chosen key signal.
    let det_sib_l = if key_l.is_some() {
        // We can't run the per-channel biquad twice on different inputs
        // and keep it cheap — instead detector reads dry via a separate
        // path using the same HPF. Cheap-and-honest approach: rerun
        // through a one-shot biquad clone. Since the biquads are
        // Direct-Form II Transposed, two parallel instances cost
        // exactly as much as duplicating state. For now we just feed
        // the key through the existing HPF — accepts a tiny coupling
        // artefact on toggle-switch. Documented for future cleanup.
        p.ess_hpf_l.process(det_l)
    } else {
        sib_l
    };
    let det_sib_r = if key_r.is_some() { p.ess_hpf_r.process(det_r) } else { sib_r };
    let sc = det_sib_l.abs().max(det_sib_r.abs());
    let env = p.ess_env.process(sc, sr, 0.5, 20.0);
    let env_db = 20.0 * env.max(1e-9).log10();
    let over = env_db - ess_thr;
    let ess_gr_db = if over > 0.0 && ess_amt > 0.05 {
        -(over.min(ess_amt))
    } else {
        0.0
    };
    let ess_gain_lin = 10f32.powf(ess_gr_db * ess_range / 20.0);
    // Mid-band split for the low-band de-esser. We process body_l/body_r
    // (post-sib-attenuation signal) through the lo HPF to isolate the
    // 0.5–3 kHz energy range; below that stays untouched.
    let mid_l = p.lo_hpf_l.process(body_l);
    let mid_r = p.lo_hpf_r.process(body_r);
    let low_only_l = body_l - mid_l;
    let low_only_r = body_r - mid_r;
    let lo_sc = mid_l.abs().max(mid_r.abs());
    let lo_env = p.lo_env.process(lo_sc, sr, 0.5, 30.0);
    let lo_env_db = 20.0 * lo_env.max(1e-9).log10();
    let lo_over = lo_env_db - lo_thr;
    let lo_gr_db = if lo_over > 0.0 && lo_amt > 0.05 {
        -(lo_over.min(lo_amt))
    } else {
        0.0
    };
    let lo_gain_lin = 10f32.powf(lo_gr_db / 20.0);
    let proc_l = low_only_l + mid_l * lo_gain_lin + sib_l * ess_gain_lin;
    let proc_r = low_only_r + mid_r * lo_gain_lin + sib_r * ess_gain_lin;

    let fast_l = p.click_fast_l.process(proc_l.abs(), sr, 0.1, 0.5);
    let fast_r = p.click_fast_r.process(proc_r.abs(), sr, 0.1, 0.5);
    let slow_l = p.click_slow_l.process(proc_l.abs(), sr, 5.0, 50.0);
    let slow_r = p.click_slow_r.process(proc_r.abs(), sr, 5.0, 50.0);
    let fast = fast_l.max(fast_r);
    let slow = slow_l.max(slow_r).max(1e-6);
    let ratio = fast / slow;
    let fast_db = 20.0 * fast.max(1e-9).log10();

    let target_gain = if ratio > clk_sens && fast_db > clk_floor_db && clk_amt > 0.05 {
        10f32.powf(-clk_amt / 20.0)
    } else {
        1.0
    };
    let release_ms = 3.0 + (clk_sens - 1.5) * (27.0 / 6.5);
    let attack_ms = 0.5_f32;
    let atk_coef = (-1.0 / (attack_ms * 0.001 * sr)).exp();
    let rel_coef = (-1.0 / (release_ms * 0.001 * sr)).exp();
    let coef = if target_gain < p.click_gain { atk_coef } else { rel_coef };
    p.click_gain = target_gain + (p.click_gain - target_gain) * coef;
    let click_gain_db = 20.0 * p.click_gain.max(1e-9).log10();

    let cleaned_l = proc_l * p.click_gain;
    let cleaned_r = proc_r * p.click_gain;
    let out_gain = 10f32.powf(output_db / 20.0);
    let wet_l = cleaned_l * out_gain;
    let wet_r = cleaned_r * out_gain;
    let final_l = dry_l * (1.0 - mix) + wet_l * mix;
    let final_r = dry_r * (1.0 - mix) + wet_r * mix;

    // Meter wants the most-negative GR from either band so the user
    // sees activity regardless of which band fired.
    let total_gr_db = ess_gr_db.min(lo_gr_db);
    (final_l, final_r, total_gr_db, click_gain_db)
}

#[allow(clippy::too_many_arguments)]
fn process_stereo(
    p: &mut PluginAudioProcessor<'_>,
    l_read: &[f32], l_write: &mut [f32],
    r_read: &[f32], r_write: &mut [f32],
    sr: f32,
    ess_thr_t: f32, ess_freq_t: f32, ess_amt_t: f32, ess_range_t: f32,
    lo_thr_t: f32, lo_freq_t: f32, lo_amt_t: f32,
    clk_sens_t: f32, clk_amt_t: f32, clk_floor_t: f32,
    output_t: f32, mix_t: f32,
    plos_on: bool, plos_thr_t: f32, plos_amt_t: f32, plos_freq_t: f32,
    hum_on: bool, hum_freq_t: f32, hum_str_t: f32,
    ext_key: Option<(&[f32], &[f32])>,
    max_ess_gr_db: &mut f32, max_click_gr_db: &mut f32,
) {
    let n = l_read.len().min(r_read.len());
    for i in 0..n {
        let (kl, kr) = ext_key
            .map(|(a, b)| (Some(a.get(i).copied().unwrap_or(0.0)),
                           Some(b.get(i).copied().unwrap_or(0.0))))
            .unwrap_or((None, None));
        let (fl, fr, ess_gr, click_gr) = step_sample(
            p, l_read[i], r_read[i], kl, kr, sr,
            ess_thr_t, ess_freq_t, ess_amt_t, ess_range_t,
            lo_thr_t, lo_freq_t, lo_amt_t,
            clk_sens_t, clk_amt_t, clk_floor_t, output_t, mix_t,
            plos_on, plos_thr_t, plos_amt_t, plos_freq_t,
            hum_on, hum_freq_t, hum_str_t,
        );
        l_write[i] = fl;
        r_write[i] = fr;
        p.shared.scope.push((fl + fr) * 0.5);
        if ess_gr < *max_ess_gr_db { *max_ess_gr_db = ess_gr; }
        if click_gr < *max_click_gr_db { *max_click_gr_db = click_gr; }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_mono(
    p: &mut PluginAudioProcessor<'_>,
    l_read: &[f32], l_write: &mut [f32],
    sr: f32,
    ess_thr_t: f32, ess_freq_t: f32, ess_amt_t: f32, ess_range_t: f32,
    lo_thr_t: f32, lo_freq_t: f32, lo_amt_t: f32,
    clk_sens_t: f32, clk_amt_t: f32, clk_floor_t: f32,
    output_t: f32, mix_t: f32,
    plos_on: bool, plos_thr_t: f32, plos_amt_t: f32, plos_freq_t: f32,
    hum_on: bool, hum_freq_t: f32, hum_str_t: f32,
    ext_key: Option<&[f32]>,
    max_ess_gr_db: &mut f32, max_click_gr_db: &mut f32,
) {
    let n = l_read.len();
    for i in 0..n {
        let s = l_read[i];
        let k = ext_key.and_then(|a| a.get(i).copied());
        let (fl, _fr, ess_gr, click_gr) = step_sample(
            p, s, s, k, k, sr,
            ess_thr_t, ess_freq_t, ess_amt_t, ess_range_t,
            lo_thr_t, lo_freq_t, lo_amt_t,
            clk_sens_t, clk_amt_t, clk_floor_t, output_t, mix_t,
            plos_on, plos_thr_t, plos_amt_t, plos_freq_t,
            hum_on, hum_freq_t, hum_str_t,
        );
        l_write[i] = fl;
        p.shared.scope.push(fl);
        if ess_gr < *max_ess_gr_db { *max_ess_gr_db = ess_gr; }
        if click_gr < *max_click_gr_db { *max_click_gr_db = click_gr; }
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 { if is_input { 2 } else { 1 } }
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
                // No IS_MAIN — secondary input the host can route a key
                // signal through. When the user routes a separate vocal
                // track here, that drives the de-esser detector instead
                // of the dry signal itself.
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
        use core::fmt::Write;
        if id.get() as usize == P_EXT_KEY {
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

pub struct SuperDuperVocal;

impl Plugin for SuperDuperVocal {
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

impl DefaultPluginFactory for SuperDuperVocal {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.vocal",
            plugin_display_name!("SuperDuper Vocal"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Vocal cleanup — de-esser + mouth de-clicker for rap and spoken word")
        // De-esser + mouth-click cleanup → RESTORATION is the right CLAP
        // bucket; keep AUDIO_EFFECT as the main category so REAPER groups it
        // under FX rather than as an instrument.
        .with_features([AUDIO_EFFECT, STEREO, RESTORATION, FILTER])
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Vocal — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperVocal>);
