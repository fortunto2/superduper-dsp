//! Level detector + gain computer: the Classic single-envelope path and the
//! Dual (program + peak) path, plus the bus-key level.
//!
//! # Where the Dual idea comes from, and what was (not) taken
//!
//! AI-NOTE: the Dual detector borrows the crest-factor scheme of a free
//! transparent mastering compressor (Tokyo Dawn Records' "Kotelnikov",
//! manual at https://docs.tokyodawn.net/kotelnikov-manual/, read 2026-10-21).
//! The name stays out of everything user-facing on purpose.
//!
//! Taken:
//! * Two cross-linked sidechain paths. An RMS (program) stage for steady
//!   material with a slower attack, and a peak stage on the rectified
//!   waveform for short events. Threshold is an RMS level.
//! * "Peak Crest": the peak path's threshold is Threshold + Peak Crest, so
//!   peaks may rise that far above the RMS-controlled level before it acts.
//!   At 0 dB both paths share one threshold.
//! * Separate releases: "Release Peak" (recovery after short overloads) and
//!   "Release RMS" (recovery after sustained reduction).
//! * Stereo sensitivity and the sidechain high-pass already exist here as
//!   Link and SC HPF, so nothing new was added for them.
//!
//! Not taken (and why):
//! * The Hilbert-transform RMS detector: a one-pole mean-square over a window
//!   tied to Attack gets the same steady-state level for far less CPU.
//! * The program-dependent release shortening/lengthening. Auto Rel already
//!   stretches the release here (applied to the RMS stage in Dual).
//! * "Equal loudness" monitoring / delta preview and "Fat" mode: monitoring
//!   and colour, not detector behaviour. Out of scope for this change.
//! * Negative Peak Crest (peak path below the program threshold): the range
//!   starts at 0 dB so the peak stage stays the transient catcher.
//!
//! How the two stages combine. The manual says "the section generating the
//! highest reduction takes control"; here they run in series, which is the
//! same whenever one stage is idle and adds them when both act:
//!
//! ```text
//!   rms_db            → curve(threshold)          = gr_rms
//!   peak_db + gr_rms  → curve(threshold + crest)  = gr_peak
//!   gr = gr_rms + gr_peak
//! ```
//!
//! The peak stage looks at what the RMS stage leaves, so on steady material
//! it only wakes when the signal's own crest factor plus the RMS stage's
//! residual overshoot ((rms − threshold)/ratio) passes Peak Crest. A steady
//! sine 9 dB over threshold at 4:1 (3 + 2.25 dB) stays under the default 9 dB;
//! a transient spike does not.

use superduper_synth_core::dsp_blocks::{compressor_gain_db_curve, CompressorCurve, EnvelopeDetector};
use superduper_synth_core::gui::LiveScope;

/// RMS window = Attack × this, but never shorter than `RMS_MIN_WINDOW_MS`:
/// the program stage must be "substantially slower" than the peak stage.
pub const RMS_ATTACK_MULT: f32 = 3.0;
pub const RMS_MIN_WINDOW_MS: f32 = 20.0;

/// The bus key reports band RMS. The Classic detector reads close to the
/// waveform peak, so the key level is lifted by a sine's crest (3 dB) to land
/// where the same signal on the input would. Exact for a sine, close enough
/// for a slow duck.
pub const KEY_CREST_DB: f32 = 3.0;

/// Per-sample detector settings (already smoothed by the caller).
#[derive(Clone, Copy, Debug)]
pub struct DetCfg {
    pub threshold: f32,
    pub ratio: f32,
    pub knee: f32,
    pub curve: CompressorCurve,
    pub attack_ms: f32,
    /// Classic release (Auto Rel already applied).
    pub release_ms: f32,
    pub dual: bool,
    pub crest_db: f32,
    pub release_peak_ms: f32,
    /// RMS-stage release (Auto Rel already applied).
    pub release_rms_ms: f32,
}

/// Gain reduction (dB, ≤ 0): total, and the two Dual stages (Classic reports
/// its whole GR as `rms_gr_db` = "program", `peak_gr_db` = 0).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DetOut {
    pub gr_db: f32,
    pub rms_gr_db: f32,
    pub peak_gr_db: f32,
}

#[derive(Default)]
pub struct Detector {
    peak: EnvelopeDetector,
    /// Dual: mean square of the detector input (linear power).
    ms: f32,
    /// Dual: smoothed RMS-stage GR in dB (attack = RMS window, release =
    /// Release RMS). Smoothing the gain, not the level, keeps the mean
    /// square symmetric so a steady tone reads its true RMS.
    rms_gr: f32,
}

#[inline]
fn coef(ms: f32, sr: f32) -> f32 { (-1.0 / (ms.max(0.01) * 0.001 * sr)).exp() }

impl Detector {
    /// One sample of the (rectified, linked, key-blended) detector input.
    #[inline]
    pub fn process(&mut self, sc: f32, c: &DetCfg, sr: f32) -> DetOut {
        if !c.dual {
            // Classic — exactly the pre-Dual code path (quality.snap pins it).
            let env = self.peak.process(sc, sr, c.attack_ms, c.release_ms);
            let env_db = 20.0 * env.max(1e-9).log10();
            let gr = compressor_gain_db_curve(env_db, c.threshold, c.ratio, c.knee, c.curve);
            return DetOut { gr_db: gr, rms_gr_db: gr, peak_gr_db: 0.0 };
        }

        // RMS (program) stage.
        let window = (c.attack_ms * RMS_ATTACK_MULT).max(RMS_MIN_WINDOW_MS);
        let x2 = sc * sc;
        self.ms = x2 + (self.ms - x2) * coef(window, sr);
        let rms_db = 10.0 * self.ms.max(1e-18).log10();
        let target = compressor_gain_db_curve(rms_db, c.threshold, c.ratio, c.knee, c.curve);
        let tc = if target < self.rms_gr { window } else { c.release_rms_ms };
        self.rms_gr = target + (self.rms_gr - target) * coef(tc, sr);
        let gr_rms = self.rms_gr;

        // Peak stage, on what the RMS stage leaves.
        let env = self.peak.process(sc, sr, c.attack_ms, c.release_peak_ms);
        let peak_db = 20.0 * env.max(1e-9).log10() + gr_rms;
        let peak_thr = c.threshold + c.crest_db.max(0.0);
        let gr_peak = compressor_gain_db_curve(peak_db, peak_thr, c.ratio, c.knee, c.curve);

        DetOut { gr_db: gr_rms + gr_peak, rms_gr_db: gr_rms, peak_gr_db: gr_peak }
    }
}

/// Number of log-spaced points the key band is sampled at.
const KEY_POINTS: usize = 8;

/// The bus key track's RMS level (dBFS) between `lo_hz` and `hi_hz`.
///
/// `key_db_at` is sampled at `KEY_POINTS` log-spaced frequencies; their mean
/// power times the number of bus bands the range spans approximates the
/// band-limited RMS (bands are RMS-per-band, so they add in power). A range
/// narrower than one band reads that band. Atomics only — audio-thread safe,
/// called once per block.
pub fn key_level_db(scope: &LiveScope, lo_hz: f32, hi_hz: f32) -> f32 {
    let (lo, hi) = (lo_hz.clamp(20.0, 20_000.0), hi_hz.clamp(20.0, 20_000.0));
    let (lo, hi) = (lo.min(hi), lo.max(hi));
    let span = (hi / lo).ln();
    let mut pow = 0.0f32;
    for k in 0..KEY_POINTS {
        let t = (k as f32 + 0.5) / KEY_POINTS as f32;
        let db = scope.key_db_at(lo * (span * t).exp());
        pow += 10f32.powf(db / 10.0);
    }
    let bands = (span / 1000f32.ln() * superduper_dsp_sdk::bus::BANDS as f32).max(1.0);
    10.0 * (pow / KEY_POINTS as f32 * bands).max(1e-12).log10()
}

/// Blend the detector input toward the key level. `amount` 0 = input only
/// (returned bit-exact, so Classic projects do not move), 1 = key only.
#[inline]
pub fn blend_key(sc: f32, key_lin: f32, amount: f32) -> f32 {
    if amount <= 0.0 { sc } else { sc + (key_lin - sc) * amount.min(1.0) }
}
