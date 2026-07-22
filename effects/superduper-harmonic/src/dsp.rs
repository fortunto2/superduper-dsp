//! SuperDuper Harmonic Clean — pitch-locked harmonic comb denoiser.
//!
//! Built to clean a **piezo-miked / electric kubyz** (jaw-harp, khomus): a
//! piezo pickup on a metal reed captures the musical harmonic vibration *and*
//! a layer of inharmonic micro-rustle — finger/contact noise, reed buzz, the
//! pickup's own hiss — that sits between the harmonics. We want to keep the
//! harmonics AND the fast plucks (the attack is the whole character of a
//! jaw-harp) while rejecting the between-harmonic noise.
//!
//! ## The idea — period-synchronous averaging (time-domain, NOT FFT)
//!
//! A pitched signal repeats every period `T = sr / f0`. Its harmonic content
//! (f0, 2·f0, 3·f0 …) is (near) identical period to period; the inharmonic
//! noise is uncorrelated period to period. So if we average the input with
//! delay-line taps at `T, 2·T, …, (K-1)·T`:
//!
//! ```text
//!   h[n] = ( x[n] + x[n-T] + x[n-2T] + … + x[n-(K-1)T] ) / K
//! ```
//!
//! the periodic content **adds coherently** (h ≈ x for harmonics) while the
//! noise **averages down** (variance ÷ K → amplitude ÷ √K). `h[n]` is a comb
//! filter: unity at every harmonic of f0, notches in between — exactly the
//! "keep the harmonics, drop the mud between them" we want. This is a live,
//! **zero-latency** IIR-free construction (all taps are *past* samples; the
//! current sample anchors the output) — no FFT window, no lookahead.
//!
//! The output subtracts a fraction `eff` of the noise residual:
//! `out = x·(1-eff) + h·eff`, i.e. `out = x - eff·(x-h)` where `(x-h)` is the
//! between-harmonic residual. `eff = Amount` in steady state.
//!
//! ## Keeping the plucks — onset-gated depth + median combine
//!
//! A pluck is a sudden broadband burst. In `h[n]` the *previous* periods don't
//! contain it yet, so the comb averages the attack down by 1/K — it would
//! **smear the pluck** at the onset. So an onset detector (fast vs slow
//! envelope) drops the comb depth toward 0 during attacks:
//! `eff = Amount·(1 - Transient·onset)`. At an onset the raw input passes
//! through clean and fast; between plucks the comb cleans the sustained drone.
//! `Transient` sets how hard attacks re-open.
//!
//! But onset gating only protects the *moment* of the attack. A **mean** comb
//! also re-injects each pluck one period later as a tap: `x[n-T]` holds the
//! pluck at time `n = onset+T`, where there is no onset, so a faint echo leaks
//! at `T, 2T, …` — and because that echo is inharmonic, on transient-heavy
//! contact-pickup material (the piezo rustle *is* clicks) it can *add*
//! between-harmonic noise instead of removing it. The fix is the **`Mode` =
//! Median** default: combine the K taps by their **median** instead of the
//! mean. Periodic content is consistent across taps → the median equals it; a
//! pluck sits in only one tap → the median discards it as an outlier. So the
//! median comb suppresses stationary between-harmonic noise AND never echoes
//! transients. `Mode = Mean` keeps the classic average (~2 dB better on pure
//! steady hiss, but it echoes plucks) for material with no transients.
//!
//! On silence / unvoiced (no confident pitch, level below the floor) the comb
//! is bypassed (`eff → 0`) so it never colours a gap or a transient-only sound.
//!
//! Pure DSP, CLAP-free, driven directly from `tests/`.

use superduper_synth_core::dsp_blocks::{
    median_small, DelayLine, EnvelopeDetector, SlewLimiter2Pole, SmoothedParam,
};
use superduper_synth_core::pitch::YinPitchTracker;

/// Fewest / most period taps in the average. `K_MIN = 2` is the gentlest comb
/// (blend with one previous period); `K_MAX = 8` is the most aggressive
/// (narrow keep-band around each harmonic → ~−9 dB noise floor).
pub const K_MIN: usize = 2;
pub const K_MAX: usize = 8;

/// Absolute lowest fundamental the tracker + comb will chase (Hz). Delay lines
/// are sized for `(K_MAX-1)` periods of this, so `Range` can go this low without
/// the comb running out of history. A kubyz bass drone sits ~73 Hz; 40 Hz gives
/// headroom for a detuned/sub electric one.
pub const RANGE_MIN_HZ: f32 = 40.0;
/// Highest tracked fundamental — a jaw-harp overtone melody rarely exceeds this,
/// and capping it keeps the tracker off high mid-band harmonics.
pub const F0_MAX_HZ: f32 = 600.0;

/// Level (peak-envelope of the mono sum) below which the source is treated as
/// silence/unvoiced and the comb is bypassed. ≈ −72 dBFS.
const VOICE_FLOOR: f32 = 2.5e-4;

/// Comb-combine `Mode` values. **Median** is the default — it is robust to
/// transient echo (see below); **Mean** is the classic period-average, ~2 dB
/// better on purely-stationary hiss but it re-injects plucks.
pub const MODE_MEDIAN: u32 = 0;
pub const MODE_MEAN: u32 = 1;

/// Map the `Bandwidth` param (0..1) to the number of period taps `K`.
/// Low Bandwidth = narrow keep-band = **aggressive** (many taps); high
/// Bandwidth = wide keep-band = **gentle** (few taps).
#[inline]
pub fn taps_from_bandwidth(bw: f32) -> usize {
    let bw = bw.clamp(0.0, 1.0);
    (K_MIN as f32 + (1.0 - bw) * (K_MAX - K_MIN) as f32)
        .round()
        .clamp(K_MIN as f32, K_MAX as f32) as usize
}

// The comb combines its K period taps by the **median** (shared
// `synth_core::dsp_blocks::median_small`, alloc-free) rather than the mean —
// that is what rejects transient echo: a pluck copied into ONE period tap is an
// outlier the median discards, while the (consistent) periodic content across
// taps is exactly the median.

/// Per-block parameter snapshot passed into `process_stereo`.
#[derive(Clone, Copy)]
pub struct HarmonicParams {
    /// Between-harmonic rejection depth, 0..1 (steady-state comb blend).
    pub amount: f32,
    /// Keep-width around each harmonic, 0..1 (→ tap count; narrow = aggressive).
    pub bandwidth: f32,
    /// Attack preservation, 0..1 (how far onsets re-open the comb).
    pub transient: f32,
    /// Dry/Wet, 0..1 (1 = fully cleaned).
    pub mix: f32,
    /// Output trim as a linear gain.
    pub output_lin: f32,
    /// Lowest fundamental to track/lock (Hz) — guards octave-down errors on a
    /// harmonic-rich source.
    pub range_hz: f32,
    /// Comb combine mode (`MODE_MEDIAN` / `MODE_MEAN`).
    pub mode: u32,
    pub bypassed: bool,
}

impl Default for HarmonicParams {
    fn default() -> Self {
        Self {
            amount: 0.7,
            bandwidth: 0.5,
            transient: 0.6,
            mix: 1.0,
            output_lin: 1.0,
            range_hz: 60.0,
            mode: MODE_MEDIAN,
            bypassed: false,
        }
    }
}

/// The denoiser. Tracks pitch on the mono sum, runs a period-synchronous comb
/// per channel. Delay lines pre-allocated at `new()`; nothing allocates in
/// `process_stereo`.
pub struct HarmonicCleaner {
    sr: f32,
    tracker: YinPitchTracker,
    /// Per-channel period-tap delay lines.
    line_l: DelayLine,
    line_r: DelayLine,
    /// Glided period `T` (samples) — a stepped period detunes the comb and
    /// clicks; the 2-pole slew turns pitch drift into a smooth comb re-tune.
    period: SlewLimiter2Pole,
    /// Fast + slow envelopes of the mono input → onset detection + voiced gate.
    fast_env: EnvelopeDetector,
    slow_env: EnvelopeDetector,
    /// Onset activity, peak-held then decayed (opens the comb on attacks).
    onset_gate: f32,
    /// Smoothed voiced/level gate (comb bypassed on silence).
    voiced_gate: f32,
    sm_amount: SmoothedParam,
    sm_transient: SmoothedParam,
    sm_mix: SmoothedParam,
    sm_output: SmoothedParam,
    /// Held f0 (Hz) — updated on each YIN hop, published for the GUI readout.
    last_f0: f32,
    /// Mean effective comb depth over the last block — the GUI "reduction" bar.
    last_reduction: f32,
    /// Max period the delay lines can serve, in samples (capacity guard).
    max_period: f32,
}

impl HarmonicCleaner {
    pub fn new(sr: f32) -> Self {
        let sr = sr.max(1.0);
        // Enough history for (K_MAX-1) periods of the lowest fundamental.
        let max_delay = ((K_MAX as f32 - 1.0) * sr / RANGE_MIN_HZ).ceil() as usize + 8;
        let max_period = sr / RANGE_MIN_HZ;
        let default_f0 = 90.0;
        Self {
            sr,
            // Window covers two periods of the lowest note (the tracker bumps it
            // up internally if needed); 256-sample hop follows drift snappily.
            tracker: YinPitchTracker::new(sr, RANGE_MIN_HZ, F0_MAX_HZ, 2048, 256, default_f0),
            line_l: DelayLine::new(max_delay),
            line_r: DelayLine::new(max_delay),
            period: SlewLimiter2Pole::new(sr / default_f0),
            fast_env: EnvelopeDetector::default(),
            slow_env: EnvelopeDetector::default(),
            onset_gate: 0.0,
            voiced_gate: 0.0,
            sm_amount: SmoothedParam::new(0.7),
            sm_transient: SmoothedParam::new(0.6),
            sm_mix: SmoothedParam::new(1.0),
            sm_output: SmoothedParam::new(1.0),
            last_f0: default_f0,
            last_reduction: 0.0,
            max_period,
        }
    }

    /// Snap the smoothers to the host-loaded initial values so the first block
    /// doesn't glide up from a default.
    pub fn prime(&mut self, amount: f32, transient: f32, mix: f32, output_lin: f32) {
        self.sm_amount.snap(amount);
        self.sm_transient.snap(transient);
        self.sm_mix.snap(mix);
        self.sm_output.snap(output_lin);
    }

    /// Last detected fundamental (Hz).
    pub fn detected_f0(&self) -> f32 {
        self.last_f0
    }

    /// Mean effective comb depth over the last processed block (0..1) — how much
    /// between-harmonic reduction is currently being applied.
    pub fn reduction(&self) -> f32 {
        self.last_reduction
    }

    /// Process one stereo block. For mono, pass `in_l` for both inputs and an
    /// empty `out_r` (`&mut []`).
    pub fn process_stereo(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
        p: &HarmonicParams,
    ) {
        let n = in_l.len().min(out_l.len());

        if p.bypassed {
            out_l[..n].copy_from_slice(&in_l[..n]);
            let rn = n.min(in_r.len()).min(out_r.len());
            out_r[..rn].copy_from_slice(&in_r[..rn]);
            return;
        }

        let sr = self.sr;
        let median = p.mode != MODE_MEAN;
        // Median needs ≥3 taps to reject an outlier (at K=2 it degenerates to the
        // mean and can't drop a transient echo), so floor it there in Median mode.
        let k = {
            let k = taps_from_bandwidth(p.bandwidth);
            if median {
                k.max(3)
            } else {
                k
            }
        };
        let range_lo = p.range_hz.clamp(RANGE_MIN_HZ, 200.0);
        let t_min = (sr / F0_MAX_HZ).max(2.0);
        // Keep (K-1)·T within the delay line's reach.
        let t_max = (sr / range_lo).min(self.max_period);
        // ~30 ms onset hold, ~20 ms voiced ramp — computed once per block.
        let onset_decay = (-1.0 / (0.030 * sr)).exp();
        let voiced_coef = 1.0 - (-1.0 / (0.020 * sr)).exp();

        let mut red_acc = 0.0f32;

        for i in 0..n {
            let xl = in_l[i];
            let xr = *in_r.get(i).unwrap_or(&xl);
            let m = (xl + xr) * 0.5;

            // ---- Pitch → glided period ------------------------------------
            if self.tracker.push(m) {
                self.last_f0 = self.tracker.current_hz();
            }
            let f0 = self.last_f0.clamp(range_lo, F0_MAX_HZ);
            let t_target = (sr / f0).clamp(t_min, t_max);
            let t = self.period.step(t_target, sr, 15.0);

            // ---- Envelopes → onset + voiced gates -------------------------
            let a = m.abs();
            let fe = self.fast_env.process(a, sr, 0.5, 6.0);
            let se = self.slow_env.process(a, sr, 12.0, 90.0);
            // Onset when the fast envelope outruns the slow one by >15 %.
            let ratio = fe / (se + 1e-4);
            let onset_now = (ratio - 1.15).clamp(0.0, 1.0);
            self.onset_gate = onset_now.max(self.onset_gate * onset_decay);
            let voiced_target = if se > VOICE_FLOOR { 1.0 } else { 0.0 };
            self.voiced_gate += (voiced_target - self.voiced_gate) * voiced_coef;

            // ---- Smoothed params ------------------------------------------
            let amount = self.sm_amount.step(p.amount, sr).clamp(0.0, 1.0);
            let transient = self.sm_transient.step(p.transient, sr).clamp(0.0, 1.0);
            let mix = self.sm_mix.step(p.mix, sr).clamp(0.0, 1.0);
            let out_lin = self.sm_output.step(p.output_lin, sr);

            // Effective comb depth: full Amount in steady state, pulled back on
            // attacks (Transient) and off on silence (voiced gate).
            let eff = amount * (1.0 - transient * self.onset_gate) * self.voiced_gate;
            red_acc += eff;

            // ---- Period-synchronous comb, per channel ---------------------
            // Gather the K period taps: tap 0 is the current sample, taps 1..K
            // are the past periods x[n-kT] (Lagrange-3 fractional reads). Then
            // combine by median (default, echo-robust) or mean (classic avg),
            // and push the current sample into the line.
            let mut taps_l = [0.0f32; K_MAX];
            let mut taps_r = [0.0f32; K_MAX];
            taps_l[0] = xl;
            taps_r[0] = xr;
            for kk in 1..k {
                let d = (kk as f32) * t;
                taps_l[kk] = self.line_l.read_lagrange3(d);
                taps_r[kk] = self.line_r.read_lagrange3(d);
            }
            let (comb_l, comb_r) = if median {
                (
                    median_small(&mut taps_l[..k]),
                    median_small(&mut taps_r[..k]),
                )
            } else {
                let inv_k = 1.0 / k as f32;
                (
                    taps_l[..k].iter().sum::<f32>() * inv_k,
                    taps_r[..k].iter().sum::<f32>() * inv_k,
                )
            };
            self.line_l.write(xl);
            self.line_r.write(xr);

            // out = x - eff·(x - comb): subtract the between-harmonic residual.
            let wet_l = xl * (1.0 - eff) + comb_l * eff;
            let wet_r = xr * (1.0 - eff) + comb_r * eff;
            let mut ol = (xl * (1.0 - mix) + wet_l * mix) * out_lin;
            let mut or_ = (xr * (1.0 - mix) + wet_r * mix) * out_lin;
            if !ol.is_finite() {
                ol = 0.0;
            }
            if !or_.is_finite() {
                or_ = 0.0;
            }
            out_l[i] = ol;
            if i < out_r.len() {
                out_r[i] = or_;
            }
        }

        if n > 0 {
            self.last_reduction = red_acc / n as f32;
        }
    }
}
