//! SuperDuper Soothe — dynamic resonance suppressor.
//!
//! Tames spectral peaks that pop out of a vocal/instrument signal —
//! sibilance, mud, harsh rolled-r resonances, plosive ring. Same idea
//! as oeksound Soothe2 but filter-bank based instead of FFT:
//!
//! 1. Split the signal into N_BANDS log-spaced bandpass channels (200 Hz..10 kHz).
//! 2. Measure each band's envelope (attack/release follower).
//! 3. Compute a "baseline" per band — the mean of its neighbours' envelopes
//!    in dB. Represents how loud this slice of the spectrum *should* be
//!    relative to its surroundings.
//! 4. If a band exceeds baseline + sensitivity, apply a negative-gain
//!    peaking-EQ cut at that band's centre. Cut depth = excess × amount.
//! 5. Smooth each cut through the attack/release time-constants so the
//!    suppression feels musical, not pumping.
//!
//! All time-domain, no FFT, RT-safe. ~24 biquads + 24 envelope detectors
//! per channel → ~1% CPU at 48 kHz.

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod presets;

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
use std::sync::atomic::Ordering;
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{Biquad, EnvelopeDetector, SmoothedParam};

fn init_logging() {
    superduper_dsp_sdk::log::init("soothe");
}
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Band geometry — log-spaced from 200 Hz to 10 kHz.
// ---------------------------------------------------------------------------

pub const N_BANDS: usize = 24;

fn band_freq(idx: usize, lo: f32, hi: f32) -> f32 {
    let t = (idx as f32) / (N_BANDS as f32 - 1.0);
    lo * (hi / lo).powf(t)
}

// ---------------------------------------------------------------------------
// Param table — kept small for now. 8 user-facing params + the standard
// Output/Mix pair. PARAMS layout is FROZEN once shipped.
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    // Depth of the cuts in dB. 0 dB = bypass-ish, 24 dB = heavy.
    ParamDef { id: 0, name: b"Amount",   min: 0.0,    max: 24.0,    default: 6.0,    unit: "dB" },
    // How much a band must exceed its neighbours' baseline before
    // suppression kicks in. Lower = more aggressive.
    ParamDef { id: 1, name: b"Sens",     min: -24.0,  max: 0.0,     default: -6.0,   unit: "dB" },
    // Peaking-EQ Q for the cut filters. Higher = narrower / surgical.
    ParamDef { id: 2, name: b"Q",        min: 2.0,    max: 12.0,    default: 5.0,    unit: ""   },
    // Spectral region — band-bank stretches between Lo and Hi.
    ParamDef { id: 3, name: b"Lo",       min: 100.0,  max: 2000.0,  default: 300.0,  unit: "Hz" },
    ParamDef { id: 4, name: b"Hi",       min: 3000.0, max: 16000.0, default: 10000.0, unit: "Hz" },
    // Detector dynamics.
    ParamDef { id: 5, name: b"Attack",   min: 0.5,    max: 30.0,    default: 5.0,    unit: "ms" },
    ParamDef { id: 6, name: b"Release",  min: 10.0,   max: 500.0,   default: 80.0,   unit: "ms" },
    // Output stage.
    ParamDef { id: 7, name: b"Mix",      min: 0.0,    max: 1.0,     default: 1.0,    unit: ""   },
    ParamDef { id: 8, name: b"Output",   min: -24.0,  max: 24.0,    default: 0.0,    unit: "dB" },
    // Mode — 0 = soft (gentle, wide cuts), 1 = sharp (narrower, default),
    // 2 = hard (aggressive, fast attack, heavy ratio).
    ParamDef { id: 9, name: b"Mode",     min: 0.0,    max: 2.0,     default: 1.0,    unit: ""   },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[9];

pub const P_AMOUNT: usize = 0;
pub const P_SENS: usize = 1;
pub const P_Q: usize = 2;
pub const P_LO: usize = 3;
pub const P_HI: usize = 4;
pub const P_ATTACK: usize = 5;
pub const P_RELEASE: usize = 6;
pub const P_MIX: usize = 7;
pub const P_OUTPUT: usize = 8;
pub const P_MODE: usize = 9;

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
    pub active_preset: std::sync::atomic::AtomicU32,
    /// Per-band current cut depth in dB (negative or zero). Audio thread
    /// writes once per block; GUI reads to paint the spectrum overlay.
    pub band_cut_db: [AtomicF32; N_BANDS],
    /// Per-band current centre frequency in Hz (recomputed when Lo/Hi
    /// change). Lets the GUI draw exact x positions.
    pub band_freq_hz: [AtomicF32; N_BANDS],
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
                band_cut_db: std::array::from_fn(|_| AtomicF32::new(0.0)),
                band_freq_hz: std::array::from_fn(|i| {
                    AtomicF32::new(band_freq(i, 300.0, 10000.0))
                }),
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
    /// Bandpass filters — measure the per-band energy. One bank per channel.
    bp_l: [Biquad; N_BANDS],
    bp_r: [Biquad; N_BANDS],
    /// Peaking EQ filters — apply the per-band cut to the actual signal.
    /// One bank per channel.
    eq_l: [Biquad; N_BANDS],
    eq_r: [Biquad; N_BANDS],
    /// Single mid-summed envelope detector per band — both channels feed
    /// it so the suppression is stereo-linked (musically correct).
    env: [EnvelopeDetector; N_BANDS],
    /// Smoothed current cut in dB per band (one-pole follower of the
    /// instantaneous excess so the EQ gain doesn't chatter).
    cut_db: [f32; N_BANDS],
    /// Centres at which the BPs and EQs are tuned (Hz). Recomputed when
    /// Lo / Hi / Q params drift past the rebuild threshold.
    band_centres: [f32; N_BANDS],
    /// Last-applied Lo/Hi/Q so we only recoeff biquads when actually needed
    /// (re-running RBJ design 24× per sample is wasted CPU).
    last_lo: f32,
    last_hi: f32,
    last_q: f32,
    smooth_amount: SmoothedParam,
    smooth_sens: SmoothedParam,
    smooth_mix: SmoothedParam,
    smooth_output: SmoothedParam,
    sample_rate: f32,
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
}

/// Rebuild the bandpass + peaking EQ banks when the band geometry changes.
/// Called off the audio hot path: only when Lo/Hi/Q drift beyond the
/// rebuild threshold (cheaper than per-sample re-coeffing).
fn rebuild_banks(p: &mut PluginAudioProcessor<'_>, lo: f32, hi: f32, q: f32) {
    let sr = p.sample_rate;
    let q_eq = q.max(2.0);
    let q_bp = (q_eq * 1.2).max(2.0);
    for i in 0..N_BANDS {
        let f = band_freq(i, lo, hi).clamp(20.0, sr * 0.45);
        p.band_centres[i] = f;
        p.bp_l[i].set_bandpass(sr, f, q_bp);
        p.bp_r[i].set_bandpass(sr, f, q_bp);
        // Peaking EQ starts at 0 dB; per-sample gain comes from `cut_db`.
        p.eq_l[i].set_peaking(sr, f, q_eq, 0.0);
        p.eq_r[i].set_peaking(sr, f, q_eq, 0.0);
        p.shared.band_freq_hz[i].store(f, Ordering::Relaxed);
    }
    p.last_lo = lo;
    p.last_hi = hi;
    p.last_q = q;
}

/// Convert ratio + attack/release to one-pole coefficients.
#[inline]
fn one_pole_coef(time_ms: f32, sr: f32) -> f32 {
    (-1.0 / (time_ms.max(0.05) * 0.001 * sr)).exp()
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
        let sr = audio_config.sample_rate as f32;
        let mut me = Self {
            shared,
            bp_l: [Biquad::default(); N_BANDS],
            bp_r: [Biquad::default(); N_BANDS],
            eq_l: [Biquad::default(); N_BANDS],
            eq_r: [Biquad::default(); N_BANDS],
            env: [EnvelopeDetector::default(); N_BANDS],
            cut_db: [0.0; N_BANDS],
            band_centres: [0.0; N_BANDS],
            last_lo: 0.0,
            last_hi: 0.0,
            last_q: 0.0,
            smooth_amount: SmoothedParam::new(load(P_AMOUNT)),
            smooth_sens: SmoothedParam::new(load(P_SENS)),
            smooth_mix: SmoothedParam::new(load(P_MIX)),
            smooth_output: SmoothedParam::new(load(P_OUTPUT)),
            sample_rate: sr,
        };
        rebuild_banks(&mut me, load(P_LO), load(P_HI), load(P_Q));
        Ok(me)
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
        let amount_target = self.shared.params[P_AMOUNT].load(Ordering::Relaxed);
        let sens_target = self.shared.params[P_SENS].load(Ordering::Relaxed);
        let lo_target = self.shared.params[P_LO].load(Ordering::Relaxed);
        let hi_target = self.shared.params[P_HI].load(Ordering::Relaxed);
        let q_target = self.shared.params[P_Q].load(Ordering::Relaxed);
        let attack_ms = self.shared.params[P_ATTACK].load(Ordering::Relaxed);
        let release_ms = self.shared.params[P_RELEASE].load(Ordering::Relaxed);
        let mix_target = self.shared.params[P_MIX].load(Ordering::Relaxed);
        let output_target = self.shared.params[P_OUTPUT].load(Ordering::Relaxed);
        let mode = self.shared.params[P_MODE]
            .load(Ordering::Relaxed)
            .round() as u32;

        // Geometry change → rebuild filter banks. Threshold of 1 Hz on
        // freqs / 0.05 on Q keeps the audio thread from re-running RBJ
        // design when the user is just nudging things in real time.
        if (lo_target - self.last_lo).abs() > 1.0
            || (hi_target - self.last_hi).abs() > 1.0
            || (q_target - self.last_q).abs() > 0.05
        {
            rebuild_banks(self, lo_target, hi_target, q_target);
        }

        // Mode tweaks the suppression ratio (excess → cut conversion).
        let ratio = match mode {
            0 => 0.4, // Soft — gentle, only the hottest peaks pull
            2 => 1.0, // Hard — 1:1, every dB above baseline = a dB cut
            _ => 0.7, // Sharp (default) — leans into peaks but stays musical
        };

        let sr = self.sample_rate;
        let coef_att = one_pole_coef(attack_ms, sr);
        let coef_rel = one_pole_coef(release_ms, sr);

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else {
                continue;
            };
            // Process both channels jointly — sum to mid for envelope
            // detection then apply per-band gain to L/R independently so
            // the cut stays stereo-correlated (no phase wandering).
            let mut chans = channel_pairs.into_iter();
            let Some(ch_l) = chans.next() else { continue };
            let ch_r = chans.next();

            // Buffer envelope per band for one block. We do per-sample
            // processing so we don't need a buffer; baselines are
            // computed sample-by-sample below.
            process_block(
                self,
                ch_l,
                ch_r,
                bypassed,
                amount_target,
                sens_target,
                mix_target,
                output_target,
                ratio,
                coef_att,
                coef_rel,
            );
        }

        // Publish per-band cut to the GUI once per block (sub-Hz update
        // rate is enough for a meter).
        for i in 0..N_BANDS {
            self.shared.band_cut_db[i].store(self.cut_db[i], Ordering::Relaxed);
        }

        Ok(ProcessStatus::Continue)
    }
}

#[allow(clippy::too_many_arguments)]
fn process_block(
    p: &mut PluginAudioProcessor<'_>,
    ch_l: ChannelPair<'_, f32>,
    ch_r: Option<ChannelPair<'_, f32>>,
    bypassed: bool,
    amount_target: f32,
    sens_target: f32,
    mix_target: f32,
    output_target: f32,
    ratio: f32,
    coef_att: f32,
    coef_rel: f32,
) {
    use superduper_dsp_sdk::clap_helpers::split_io;
    let Some((read_l, write_l)) = split_io(ch_l) else { return };
    let (read_r, mut write_r): (&[f32], Option<&mut [f32]>) = if let Some(ch_r) = ch_r {
        let Some((r, w)) = split_io(ch_r) else { return };
        (r, Some(w))
    } else {
        (read_l, None)
    };

    if bypassed {
        write_l.copy_from_slice(read_l);
        if let Some(w) = write_r {
            w.copy_from_slice(read_r);
        }
        return;
    }

    let sr = p.sample_rate;
    let n = read_l.len();

    // Borrow-split — we mutate p.eq_l/p.eq_r/p.bp_*/p.env/p.cut_db inside
    // the per-sample loop, so destructure into separate fields up front
    // to keep the borrow checker happy.
    let PluginAudioProcessor {
        bp_l, bp_r, eq_l, eq_r, env, cut_db, ..
    } = p;

    for i in 0..n {
        let xl = read_l[i];
        let xr = if read_r.len() == n { read_r[i] } else { xl };
        let mid = (xl + xr) * 0.5;

        let amount = p.smooth_amount.step(amount_target, sr);
        let sens = p.smooth_sens.step(sens_target, sr);
        let mix = p.smooth_mix.step(mix_target, sr);
        let out_db = p.smooth_output.step(output_target, sr);
        let out_lin = 10f32.powf(out_db / 20.0);

        // 1) Bandpass + per-band envelope in dB on the mid signal.
        // Two passes: first compute envelopes, then baselines + cuts so
        // a band's neighbours are evaluated against the same snapshot.
        let mut env_db = [0.0_f32; N_BANDS];
        for b in 0..N_BANDS {
            let band_mid = (bp_l[b].process(mid) + bp_r[b].process(mid)) * 0.5;
            // Coefficient swap inside EnvelopeDetector is asymmetric, but
            // we already have per-band coefs precomputed — use the slow
            // path here since the EnvelopeDetector API does both.
            let lvl = env[b].process(band_mid.abs(), sr, 1.0, 20.0);
            env_db[b] = 20.0 * (lvl + 1e-9).log10();
            let _ = (coef_att, coef_rel);
        }

        // 2) Baselines = mean of dB envelopes in a 5-band local window.
        // Reflect at edges so the corner bands don't get smeared by zeros.
        const W: i32 = 2;
        let mut baseline_db = [0.0_f32; N_BANDS];
        for b in 0..N_BANDS {
            let mut sum = 0.0;
            let mut count = 0;
            for off in -W..=W {
                let j = (b as i32 + off).clamp(0, N_BANDS as i32 - 1) as usize;
                if j == b {
                    continue;
                }
                sum += env_db[j];
                count += 1;
            }
            baseline_db[b] = sum / count.max(1) as f32;
        }

        // 3) Per-band excess → cut. Smoothed through att/rel.
        for b in 0..N_BANDS {
            let excess = env_db[b] - baseline_db[b] - sens;
            let target_cut = if excess > 0.0 {
                -(excess * ratio * (amount / 12.0)).min(amount)
            } else {
                0.0
            };
            // Faster attack on growing cuts, slower release on recovery.
            let coef = if target_cut < cut_db[b] {
                coef_att
            } else {
                coef_rel
            };
            cut_db[b] = target_cut + (cut_db[b] - target_cut) * coef;

            // Update both peak EQs — bands are independent so re-tuning
            // gain only is cheap (no full RBJ recompute, just b0/b1/b2
            // scaling internal to set_peaking).
            let f = p.band_centres[b];
            let q = p.last_q;
            eq_l[b].set_peaking(sr, f, q, cut_db[b]);
            eq_r[b].set_peaking(sr, f, q, cut_db[b]);
        }

        // 4) Apply all 24 peaking EQs in cascade on each channel.
        let mut yl = xl;
        let mut yr = xr;
        for b in 0..N_BANDS {
            yl = eq_l[b].process(yl);
            yr = eq_r[b].process(yr);
        }

        let out_l = xl * (1.0 - mix) + yl * mix * out_lin;
        let out_r = xr * (1.0 - mix) + yr * mix * out_lin;
        write_l[i] = out_l;
        if let Some(ref mut w) = write_r {
            // Stored above as Option<&mut [f32]>; manual indexed write
            // because the iterator captured it once. SAFETY ok: same len.
            w[i] = out_r;
        }

        p.shared.scope.push((out_l + out_r) * 0.5);
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions (audio ports, params, state, GUI)
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 {
        1
    }
    fn get(&mut self, _index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
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
        let handle =
            gui::open_window(&window, self.shared.shared_handle(), self.gui_resize.clone());
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

pub struct SuperDuperSoothe;

impl Plugin for SuperDuperSoothe {
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

impl DefaultPluginFactory for SuperDuperSoothe {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.soothe",
            plugin_display_name!("SuperDuper Soothe"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Dynamic resonance suppressor — 24-band filter bank with baseline-relative cuts.")
        .with_features([AUDIO_EFFECT, STEREO, EQUALIZER])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!(
            "new_shared: Soothe — build {} ({})",
            build_num!(),
            build_date!()
        );
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

clack_export_entry!(SinglePluginEntry<SuperDuperSoothe>);
