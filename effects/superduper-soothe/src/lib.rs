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
    // Bus key (Trackspacer): with a key track picked, each band is cut by up
    // to Key Depth as the key gets loud in that band. 0 = off, so every
    // preset and project from before sounds the same.
    ParamDef { id: 10, name: b"Key Depth", min: 0.0,   max: 24.0,    default: 0.0,    unit: "dB" },
    // How far above −50 dB (the bus's "loud" line) the key must be in a
    // band for the full depth: small = ducks on any trace of the key.
    ParamDef { id: 11, name: b"Key Range", min: 6.0,   max: 40.0,    default: 24.0,   unit: "dB" },
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
pub const P_KEY_DEPTH: usize = 10;
pub const P_KEY_RANGE: usize = 11;
/// The key's level that starts the duck: the bus's "loud" line.
const KEY_FLOOR_DB: f32 = -50.0;

/// Per-band cut from the bus key: 0 below the floor, −depth at floor + range.
pub fn key_cut_db(key_db: f32, depth: f32, range: f32) -> f32 {
    -depth * ((key_db - KEY_FLOOR_DB) / range.max(1.0)).clamp(0.0, 1.0)
}

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
                scope: superduper_synth_core::gui::LiveScope::new(4096),
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
    engine: Engine,
}

/// Per-block control values, read from the params once per block.
#[derive(Clone, Copy, Debug)]
pub struct BlockParams {
    pub amount: f32,
    pub sens: f32,
    pub mix: f32,
    pub output_db: f32,
    /// Mode's excess → cut ratio (Soft 0.4 / Sharp 0.7 / Hard 1.0).
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
}

/// The suppressor's DSP, free of CLAP so tests drive it directly: the
/// bandpass bank, the per-band envelopes and cuts, the peaking-EQ bank and
/// the key-cut compensation.
pub struct Engine {
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
    /// Per-band scale on the key cut so a uniform key cut measures its
    /// depth at every centre, see `key_scales`.
    key_scale: [f32; N_BANDS],
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

/// How much of a uniform key cut each band centre receives. The 24 peaking
/// cuts cascade and their skirts overlap (0.245 octave apart at the default
/// range), so with every band cut by D dB a band centre measures
/// D × Σ_b leak[b], leak[b] being band b's cut there per dB of its own:
/// −25.7 dB for D = 12 at Q 5 and −55 dB at Q 2, measured on the cascade.
/// Returns the per-band scale s with Σ_b leak[k][b]·s[b] = 1 at every
/// centre k; the key cut is multiplied by it, so a uniform key cut of D dB
/// measures D ± 0.5 dB at every centre at Q 2, 5 and 12 (measured, 24
/// bands, white noise). Evaluated at a 1 dB cut: the dB response is close
/// to linear in the cut, and the scaled per-band cuts are smallest exactly
/// where the leak is largest. The scale is built for a broadband key; a key
/// that is loud in one band alone is cut by s·D there (0.5·D at Q 5), since
/// its neighbours contribute nothing — the price of not over-cutting a
/// broadband key by 14 dB. 24 × 24 magnitude evaluations and one 24 × 24
/// elimination, run only when the banks are rebuilt.
fn key_scales(centres: &[f32; N_BANDS], q: f32, sr: f32) -> [f32; N_BANDS] {
    // leak[k][b]: band b's cut at centre k per dB of its own (≥ 0, 1 at k = b).
    let mut leak = [[0.0f32; N_BANDS]; N_BANDS];
    let mut bq = Biquad::default();
    for (b, &f) in centres.iter().enumerate() {
        bq.set_peaking(sr, f, q, -1.0);
        for (k, &fk) in centres.iter().enumerate() {
            leak[k][b] = -bq.magnitude_db_at(fk, sr);
        }
    }
    // Solve leak · s = 1: with every band cut by D·s[b], centre k measures
    // D · Σ_b leak[k][b]·s[b] = D. Dividing by the row sum alone is exact
    // only while the cuts stay uniform, and they do not once scaled (Q 2
    // read −9.9 dB on one band for a 12 dB key that way).
    let row_sum: [f32; N_BANDS] = std::array::from_fn(|k| leak[k].iter().sum::<f32>().max(1.0));
    let mut s = [1.0f32; N_BANDS];
    if !solve_in_place(&mut leak, &mut s) || s.iter().any(|v| !v.is_finite() || *v <= 0.0) {
        // Singular or ringing (never seen at Q 2..12): the row-sum scale.
        return std::array::from_fn(|k| 1.0 / row_sum[k]);
    }
    s
}

/// Gaussian elimination with partial pivoting on `a`, solution in `b`.
/// False if singular.
fn solve_in_place(a: &mut [[f32; N_BANDS]; N_BANDS], b: &mut [f32; N_BANDS]) -> bool {
    for col in 0..N_BANDS {
        let p = (col..N_BANDS).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs())).unwrap_or(col);
        if a[p][col].abs() < 1e-9 {
            return false;
        }
        a.swap(col, p);
        b.swap(col, p);
        for row in col + 1..N_BANDS {
            let f = a[row][col] / a[col][col];
            if f != 0.0 {
                for j in col..N_BANDS {
                    a[row][j] -= f * a[col][j];
                }
                b[row] -= f * b[col];
            }
        }
    }
    for col in (0..N_BANDS).rev() {
        let mut v = b[col];
        for j in col + 1..N_BANDS {
            v -= a[col][j] * b[j];
        }
        b[col] = v / a[col][col];
    }
    true
}

/// Convert ratio + attack/release to one-pole coefficients.
#[inline]
fn one_pole_coef(time_ms: f32, sr: f32) -> f32 {
    (-1.0 / (time_ms.max(0.05) * 0.001 * sr)).exp()
}

impl Engine {
    /// A bank spanning `lo`..`hi` Hz with peaking Q `q`; the smoothed params
    /// start at the given values.
    #[allow(clippy::too_many_arguments)]
    pub fn new(sr: f32, lo: f32, hi: f32, q: f32, amount: f32, sens: f32, mix: f32, output_db: f32) -> Self {
        let mut e = Self {
            bp_l: [Biquad::default(); N_BANDS],
            bp_r: [Biquad::default(); N_BANDS],
            eq_l: [Biquad::default(); N_BANDS],
            eq_r: [Biquad::default(); N_BANDS],
            env: [EnvelopeDetector::default(); N_BANDS],
            cut_db: [0.0; N_BANDS],
            band_centres: [0.0; N_BANDS],
            key_scale: [1.0; N_BANDS],
            last_lo: 0.0,
            last_hi: 0.0,
            last_q: 0.0,
            smooth_amount: SmoothedParam::new(amount),
            smooth_sens: SmoothedParam::new(sens),
            smooth_mix: SmoothedParam::new(mix),
            smooth_output: SmoothedParam::new(output_db),
            sample_rate: sr,
        };
        e.rebuild_banks(lo, hi, q);
        e
    }

    pub fn band_centres(&self) -> &[f32; N_BANDS] {
        &self.band_centres
    }
    pub fn cut_db(&self) -> &[f32; N_BANDS] {
        &self.cut_db
    }
    pub fn key_scale(&self) -> &[f32; N_BANDS] {
        &self.key_scale
    }

    /// Geometry change → rebuild filter banks. Threshold of 1 Hz on
    /// freqs / 0.05 on Q keeps the audio thread from re-running RBJ
    /// design when the user is just nudging things in real time.
    pub fn geometry_moved(&self, lo: f32, hi: f32, q: f32) -> bool {
        (lo - self.last_lo).abs() > 1.0 || (hi - self.last_hi).abs() > 1.0 || (q - self.last_q).abs() > 0.05
    }

    /// Rebuild the bandpass + peaking EQ banks when the band geometry changes.
    /// Called off the audio hot path: only when Lo/Hi/Q drift beyond the
    /// rebuild threshold (cheaper than per-sample re-coeffing).
    pub fn rebuild_banks(&mut self, lo: f32, hi: f32, q: f32) {
        let sr = self.sample_rate;
        let q_eq = q.max(2.0);
        let q_bp = (q_eq * 1.2).max(2.0);
        for i in 0..N_BANDS {
            let f = band_freq(i, lo, hi).clamp(20.0, sr * 0.45);
            self.band_centres[i] = f;
            self.bp_l[i].set_bandpass(sr, f, q_bp);
            self.bp_r[i].set_bandpass(sr, f, q_bp);
            // Peaking EQ starts at 0 dB; per-sample gain comes from `cut_db`.
            self.eq_l[i].set_peaking(sr, f, q_eq, 0.0);
            self.eq_r[i].set_peaking(sr, f, q_eq, 0.0);
        }
        self.key_scale = key_scales(&self.band_centres, q_eq, sr);
        self.last_lo = lo;
        self.last_hi = hi;
        self.last_q = q;
    }

    /// The bus key's cut per band for this block, compensated for the
    /// cascade (`key_scales`). `key_db_at` is the key's level at a frequency.
    pub fn key_cuts(&self, key_db_at: impl Fn(f32) -> f32, depth: f32, range: f32) -> [f32; N_BANDS] {
        if depth <= 0.0 {
            return [0.0; N_BANDS];
        }
        std::array::from_fn(|b| key_cut_db(key_db_at(self.band_centres[b]), depth, range) * self.key_scale[b])
    }

    /// One block. `read_r` is `read_l` again (or shorter) when mono; the
    /// right output is written when present.
    #[allow(clippy::too_many_arguments)]
    pub fn process_block(
        &mut self,
        read_l: &[f32],
        read_r: &[f32],
        write_l: &mut [f32],
        mut write_r: Option<&mut [f32]>,
        p: &BlockParams,
        key_cut: &[f32; N_BANDS],
        scope: Option<&superduper_synth_core::gui::LiveScope>,
    ) {
        let sr = self.sample_rate;
        let n = read_l.len();
        let coef_att = one_pole_coef(p.attack_ms, sr);
        let coef_rel = one_pole_coef(p.release_ms, sr);

        // Borrow-split — we mutate eq_l/eq_r/bp_*/env/cut_db inside the
        // per-sample loop, so destructure into separate fields up front
        // to keep the borrow checker happy.
        let Engine { bp_l, bp_r, eq_l, eq_r, env, cut_db, band_centres, last_q, .. } = self;
        let q = *last_q;

        for i in 0..n {
            let xl = read_l[i];
            let xr = if read_r.len() == n { read_r[i] } else { xl };
            let mid = (xl + xr) * 0.5;

            let amount = self.smooth_amount.step(p.amount, sr);
            let sens = self.smooth_sens.step(p.sens, sr);
            let mix = self.smooth_mix.step(p.mix, sr);
            let out_db = self.smooth_output.step(p.output_db, sr);
            let out_lin = 10f32.powf(out_db / 20.0);

            // 1) Bandpass + per-band envelope in dB on the mid signal.
            // Two passes: first compute envelopes, then baselines + cuts so
            // a band's neighbours are evaluated against the same snapshot.
            let mut env_db = [0.0_f32; N_BANDS];
            for b in 0..N_BANDS {
                let band_mid = (bp_l[b].process(mid) + bp_r[b].process(mid)) * 0.5;
                // The EnvelopeDetector does its own coefficient swap; the
                // precomputed per-band coefs drive the cut smoothing below.
                let lvl = env[b].process(band_mid.abs(), sr, 1.0, 20.0);
                env_db[b] = 20.0 * (lvl + 1e-9).log10();
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
                let resonance_cut = if excess > 0.0 {
                    -(excess * p.ratio * (amount / 12.0)).min(amount)
                } else {
                    0.0
                };
                // The key's duck (already scaled for the cascade) adds to the
                // resonance cut, through the same attack/release.
                let target_cut = (resonance_cut + key_cut[b]).max(-48.0);
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
                let f = band_centres[b];
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
                w[i] = out_r;
            }

            if let Some(sc) = scope {
                sc.push((out_l + out_r) * 0.5);
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
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        shared.scope.set_sample_rate(audio_config.sample_rate as f32);
        slog!("activate sr={}", audio_config.sample_rate);
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        let sr = audio_config.sample_rate as f32;
        let engine = Engine::new(
            sr, load(P_LO), load(P_HI), load(P_Q),
            load(P_AMOUNT), load(P_SENS), load(P_MIX), load(P_OUTPUT),
        );
        for (i, f) in engine.band_centres().iter().enumerate() {
            shared.band_freq_hz[i].store(*f, Ordering::Relaxed);
        }
        Ok(Self { shared, engine })
    }

    fn process(
        &mut self,
        process: Process,
        mut audio: Audio,
        mut events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // Events, dirty/gesture flags, bypass (which still feeds the bus
        // scope) and the port walk live in synth-core::effect.
        let shared = self.shared;
        let _denormals = superduper_synth_core::effect::begin_events(&**shared, &process, &mut events);

        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        let (lo_target, hi_target, q_target) = (load(P_LO), load(P_HI), load(P_Q));
        let mode = load(P_MODE).round() as u32;

        if self.engine.geometry_moved(lo_target, hi_target, q_target) {
            self.engine.rebuild_banks(lo_target, hi_target, q_target);
            for (i, f) in self.engine.band_centres().iter().enumerate() {
                shared.band_freq_hz[i].store(*f, Ordering::Relaxed);
            }
        }

        let bp = BlockParams {
            amount: load(P_AMOUNT),
            sens: load(P_SENS),
            mix: load(P_MIX),
            output_db: load(P_OUTPUT),
            // Mode tweaks the suppression ratio (excess → cut conversion).
            ratio: match mode {
                0 => 0.4, // Soft — gentle, only the hottest peaks pull
                2 => 1.0, // Hard — 1:1, every dB above baseline = a dB cut
                _ => 0.7, // Sharp (default) — leans into peaks but stays musical
            },
            attack_ms: load(P_ATTACK),
            release_ms: load(P_RELEASE),
        };

        // Bus key, once per block: atomics only (the publisher thread fills
        // them ~20×/s, so finer resolution would buy nothing).
        let key_cut = self.engine.key_cuts(|f| shared.scope.key_db_at(f), load(P_KEY_DEPTH), load(P_KEY_RANGE));

        let engine = &mut self.engine;
        superduper_synth_core::effect::run_ports(&**shared, &mut audio, |l_read, l_write, r| {
            // A mono port feeds the left channel to both sides of the engine.
            let (read_r, write_r): (&[f32], Option<&mut [f32]>) = match r {
                Some((rr, rw)) => (rr, Some(rw)),
                None => (l_read, None),
            };
            engine.process_block(l_read, read_r, l_write, write_r, &bp, &key_cut, None);
        })?;

        // Publish per-band cut to the GUI once per block (sub-Hz update
        // rate is enough for a meter).
        for (i, c) in self.engine.cut_db().iter().enumerate() {
            shared.band_cut_db[i].store(*c, Ordering::Relaxed);
        }

        Ok(ProcessStatus::Continue)
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

impl PluginMainThread<'_> {
    /// What a parameter value reads as — the one place for it, so the host's
    /// text entry (`text_to_value`) accepts exactly what the display shows.
    fn display_text<W: core::fmt::Write>(&mut self, id: ClapId, v: f64, w: &mut W) -> core::fmt::Result {
        ParamDef::write_display(PARAMS, id, v, w)
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
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::track_info::PluginTrackInfo>();
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
        let shared = PluginShared::new();
        superduper_synth_core::gui::spawn_bus_publisher_mcp("SuperDuper Soothe", &shared.inner, |s| &s.scope, Some(superduper_synth_core::gui::ParamsAccess { defs: PARAMS, values: |s| &s.params, dirty: Some(|s| &s.dirty_params) }), None);
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

clack_export_entry!(SinglePluginEntry<SuperDuperSoothe>);

impl clack_extensions::track_info::PluginTrackInfoImpl for PluginMainThread<'_> {
    fn changed(&self) {
        self.track_watch.refresh();
    }
}

#[cfg(test)]
mod key_tests {
    use super::key_cut_db;

    #[test]
    fn key_duck_scales_between_floor_and_range() {
        assert_eq!(key_cut_db(-80.0, 12.0, 24.0), 0.0, "a quiet key does nothing");
        assert_eq!(key_cut_db(-50.0, 12.0, 24.0), 0.0);
        assert!((key_cut_db(-38.0, 12.0, 24.0) + 6.0).abs() < 1e-5, "halfway up the range: half the depth");
        assert_eq!(key_cut_db(-10.0, 12.0, 24.0), -12.0, "full depth, never more");
    }
}
