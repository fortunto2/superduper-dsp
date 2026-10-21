//! Timbre analysis and morph gains — the DSP behind SuperDuper Morph.
//!
//! Spectral modelling without a neural net. Per STFT frame a signal is reduced
//! to a [`FrameAnalysis`]: f0 (from the caller's pitch tracker), a voicing
//! decision, the level of each harmonic `k·f0` (k ≤ [`MAX_HARM`]), and a smooth
//! spectral envelope sampled on a fixed log-frequency grid ([`GRID`] points,
//! 40 Hz–16 kHz, so it is sample-rate independent and small enough to persist).
//!
//! A target timbre is a [`TargetFrame`]: harmonic levels *by harmonic number*
//! (so a target at one pitch transfers to a voice at another) plus an envelope.
//! It is built from a measured [`TimbreProfile`] (a live sidechain or a frozen
//! capture) or from the parametric Kubyz model ([`ModelTarget`]).
//!
//! [`morph_gains`] turns voice + target + Morph into one real gain per FFT bin.
//! The voice's own spectrum is multiplied by it — phases, timing, noise and
//! consonants are the voice's; only the magnitudes of its harmonics move toward
//! the target's in dB. That choice over additive resynthesis is deliberate:
//! Morph = 0 is an exact identity (gain 0 dB everywhere), there are no
//! oscillator phases to keep coherent on a glide, the output pitch IS the input
//! pitch, and stereo comes free (one gain curve, applied to L and R).
//! The cost: a harmonic the voice does not have cannot be created, only raised
//! from whatever is in its bins — capped at [`GAIN_MAX_DB`].
//!
//! Vocoder relation: SuperDuper Vocoder's Spectral mode imposes the voice's
//! smoothed envelope on the carrier's fine structure *at the carrier's pitch*.
//! Morph at Morph = 1, Vowels = 1 computes target harmonic `k` as the target's
//! source level (its harmonic level with its own envelope divided out) times
//! the voice's envelope at `k·f0_voice` — the same cross-synthesis, except the
//! fine structure is re-indexed by harmonic number onto the voice's pitch, so
//! the voice keeps its melody instead of taking the carrier's.
//!
//! RT-safe: buffers are allocated in [`Analyzer::new`] / [`GainScratch::new`];
//! nothing allocates per frame.

use crate::kubyz::formant_bank::{bandpass_response, effective_q, FormantBands, MAKEUP};
use crate::kubyz::N_HARMONICS;
use realfft::num_complex::Complex;

/// Harmonics tracked per frame (also bounded by Nyquist and 16 kHz).
pub const MAX_HARM: usize = 40;
/// Log-frequency envelope grid size and span.
pub const GRID: usize = 96;
pub const GRID_LO: f32 = 40.0;
pub const GRID_HI: f32 = 16_000.0;
/// "Nothing there" level in dB.
pub const FLOOR_DB: f32 = -120.0;
/// Per-bin gain limits. +30 dB raises a weak voice harmonic toward a loud
/// target one; past that it is amplifying noise, not a harmonic.
pub const GAIN_MAX_DB: f32 = 30.0;
pub const GAIN_MIN_DB: f32 = -60.0;
/// Frames quieter than this (dB re a full-scale sine) are passed untouched.
pub const SILENCE_DB: f32 = -80.0;
/// Below this a frame is never called voiced.
const VOICED_MIN_DB: f32 = -60.0;
/// Harmonicity (0 = noise, 1 = pure harmonic) needed to call a frame voiced.
pub const VOICED_HARMONICITY: f32 = 0.5;
/// Envelope-mode Vowels smoothing: ±6 grid points ≈ ±0.6 octave.
const COARSE_W: usize = 6;

#[inline]
pub fn grid_hz(i: usize) -> f32 {
    GRID_LO * (GRID_HI / GRID_LO).powf(i as f32 / (GRID - 1) as f32)
}

/// Linear interpolation of a grid curve at `f` Hz (clamped at the ends).
#[inline]
pub fn grid_at(env: &[f32; GRID], f: f32) -> f32 {
    let pos = ((f.max(1.0) / GRID_LO).ln() / (GRID_HI / GRID_LO).ln()) * (GRID - 1) as f32;
    let pos = pos.clamp(0.0, (GRID - 1) as f32);
    let i = (pos as usize).min(GRID - 2);
    let t = pos - i as f32;
    env[i] + (env[i + 1] - env[i]) * t
}

#[inline]
fn db_pow(p: f32) -> f32 {
    10.0 * (p.max(1e-30)).log10()
}

/// `10·log10(Σ 10^(x/10))` — the total power of a dB curve, in dB.
pub fn power_sum_db(x: &[f32]) -> f32 {
    let s: f32 = x.iter().map(|v| 10f32.powf(v * 0.1)).sum();
    db_pow(s)
}

/// One frame of one signal.
#[derive(Clone, Copy, Debug)]
pub struct FrameAnalysis {
    pub f0: f32,
    pub voiced: bool,
    /// Frame level in dB re the frame of a full-scale sine.
    pub level_db: f32,
    /// 0 = noise, 1 = all energy on the harmonics of `f0`.
    pub harmonicity: f32,
    /// Harmonics measured (`k·f0` below the analysis ceiling).
    pub n_harm: usize,
    /// Absolute harmonic levels (dB, peak-bin power, parabolic-interpolated).
    pub harm_db: [f32; MAX_HARM],
    /// Smoothed spectral envelope on the grid (dB, mean power per bin).
    pub env_db: [f32; GRID],
}

impl Default for FrameAnalysis {
    fn default() -> Self {
        Self {
            f0: 0.0,
            voiced: false,
            level_db: FLOOR_DB,
            harmonicity: 0.0,
            n_harm: 0,
            harm_db: [FLOOR_DB; MAX_HARM],
            env_db: [FLOOR_DB; GRID],
        }
    }
}

/// Per-signal analysis scratch. One per analysed stream.
pub struct Analyzer {
    pow: Box<[f32]>,
    env: Box<[f32]>,
    prefix: Box<[f64]>,
    fpb: f32,
    half: usize,
    /// Power of a full-scale sine's frame: `N²·3/32` (Hann, one-sided).
    ref_pow: f32,
}

impl Analyzer {
    /// `half` = N/2 (bins are `0..=half`), `fpb` = Hz per bin.
    pub fn new(half: usize, fpb: f32) -> Self {
        let n = (2 * half) as f32;
        Self {
            pow: vec![0.0; half + 1].into_boxed_slice(),
            env: vec![0.0; half + 1].into_boxed_slice(),
            prefix: vec![0.0; half + 2].into_boxed_slice(),
            fpb,
            half,
            ref_pow: n * n * 3.0 / 32.0,
        }
    }

    /// Power spectrum of the last analysed frame.
    pub fn power(&self) -> &[f32] {
        &self.pow
    }

    /// Highest harmonic frequency analysed.
    fn ceiling_hz(&self) -> f32 {
        (self.half as f32 * self.fpb * 0.9).min(GRID_HI)
    }

    /// Analyse one spectrum. `f0_hint` comes from the caller's pitch tracker
    /// (which holds its last value on unvoiced input — the harmonicity test
    /// below is what catches that).
    pub fn analyze(&mut self, spec: &[Complex<f32>], f0_hint: f32, out: &mut FrameAnalysis) {
        let half = self.half.min(spec.len().saturating_sub(1));
        let mut total = 0.0f64;
        for b in 0..=half {
            let p = spec[b].re * spec[b].re + spec[b].im * spec[b].im;
            self.pow[b] = p;
            if b > 0 {
                total += p as f64;
            }
        }
        out.level_db = db_pow(total as f32 / self.ref_pow);
        let f0 = f0_hint.clamp(30.0, 2000.0);
        out.f0 = f0;
        self.measure_harmonics(f0, out);
        self.envelope(f0, out);
        out.voiced = out.level_db > VOICED_MIN_DB
            && out.harmonicity > VOICED_HARMONICITY
            && (50.0..=1000.0).contains(&f0_hint);
    }

    /// Harmonic peaks + harmonicity (energy on the harmonic lobes, with the
    /// share a flat noise spectrum would put there subtracted out).
    fn measure_harmonics(&mut self, f0: f32, out: &mut FrameAnalysis) {
        let spacing = f0 / self.fpb;
        let lobe = (spacing * 0.3).clamp(1.0, 2.0);
        let ceiling = self.ceiling_hz();
        let mut n = 0;
        let mut on = 0.0f64;
        for k in 1..=MAX_HARM {
            let fk = k as f32 * f0;
            if fk > ceiling {
                break;
            }
            let c = fk / self.fpb;
            out.harm_db[k - 1] = self.peak_db(c);
            let lo = (c - lobe).ceil().max(1.0) as usize;
            let hi = ((c + lobe).floor() as usize).min(self.half);
            for b in lo..=hi {
                on += self.pow[b] as f64;
            }
            n = k;
        }
        for h in out.harm_db.iter_mut().skip(n) {
            *h = FLOOR_DB;
        }
        out.n_harm = n;
        if n == 0 {
            out.harmonicity = 0.0;
            return;
        }
        let lo = (0.5 * spacing).ceil().max(1.0) as usize;
        let hi = (((n as f32 + 0.5) * spacing) as usize).min(self.half);
        let span: f64 = self.pow[lo..=hi.max(lo)].iter().map(|&p| p as f64).sum();
        let r = if span > 0.0 { (on / span) as f32 } else { 0.0 };
        let r0 = ((2.0 * lobe + 1.0) / spacing).min(0.95);
        out.harmonicity = ((r - r0) / (1.0 - r0)).clamp(0.0, 1.0);
    }

    /// Peak power (dB) near fractional bin `c`, parabolic on the dB values.
    fn peak_db(&self, c: f32) -> f32 {
        let ci = c.round() as isize;
        let mut best = (ci, -1.0f32);
        for b in (ci - 1)..=(ci + 1) {
            if b >= 1 && (b as usize) < self.half && self.pow[b as usize] > best.1 {
                best = (b, self.pow[b as usize]);
            }
        }
        let b = best.0.clamp(1, self.half as isize - 1) as usize;
        let (a, m, z) = (db_pow(self.pow[b - 1]), db_pow(self.pow[b]), db_pow(self.pow[b + 1]));
        let den = a - 2.0 * m + z;
        if den < -1e-6 {
            let d = (0.5 * (a - z) / den).clamp(-0.5, 0.5);
            m - 0.25 * (a - z) * d
        } else {
            m
        }
    }

    /// Mean power per bin over a window wide enough to bridge the harmonic
    /// comb (≥ one f0 spacing) and growing with frequency (constant-Q-ish),
    /// via a prefix sum — O(bins). Sampled onto the grid.
    fn envelope(&mut self, f0: f32, out: &mut FrameAnalysis) {
        let half = self.half;
        self.prefix[0] = 0.0;
        for b in 0..=half {
            self.prefix[b + 1] = self.prefix[b] + self.pow[b] as f64;
        }
        let min_w = (f0.max(150.0) / self.fpb).ceil() as usize;
        for b in 0..=half {
            let w = min_w.max((b as f32 * 0.12) as usize).max(1);
            let lo = b.saturating_sub(w);
            let hi = (b + w).min(half);
            self.env[b] = ((self.prefix[hi + 1] - self.prefix[lo]) / (hi - lo + 1) as f64) as f32;
        }
        for (g, e) in out.env_db.iter_mut().enumerate() {
            let pos = (grid_hz(g) / self.fpb).min(half as f32);
            let i = (pos as usize).min(half.saturating_sub(1));
            let t = pos - i as f32;
            let p = self.env[i] + (self.env[i + 1] - self.env[i]) * t;
            *e = db_pow(p / self.ref_pow);
        }
    }
}

/// A target timbre that can be stored: harmonic levels by number (energy-
/// normalised, dB), the pitch they were measured at, an envelope on the grid
/// (energy-normalised, dB) and the noise share. Live sidechain and Capture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimbreProfile {
    pub f0: f32,
    pub harm_valid: bool,
    pub harm_db: [f32; MAX_HARM],
    pub env_valid: bool,
    pub env_db: [f32; GRID],
    /// 1 − harmonicity, smoothed.
    pub noise: f32,
}

impl Default for TimbreProfile {
    fn default() -> Self {
        Self {
            f0: 0.0,
            harm_valid: false,
            harm_db: [FLOOR_DB; MAX_HARM],
            env_valid: false,
            env_db: [FLOOR_DB; GRID],
            noise: 1.0,
        }
    }
}

impl TimbreProfile {
    /// Fold one analysed frame in. `a` = smoothing (0 = jump to the frame).
    /// Harmonics only update on voiced frames, the envelope on any audible
    /// one — so the target's timbre holds between its notes.
    pub fn update(&mut self, fa: &FrameAnalysis, a: f32) {
        if fa.level_db > VOICED_MIN_DB {
            let norm = power_sum_db(&fa.env_db);
            for (e, &x) in self.env_db.iter_mut().zip(fa.env_db.iter()) {
                let x = (x - norm).max(FLOOR_DB);
                *e = if self.env_valid { x + (*e - x) * a } else { x };
            }
            self.env_valid = true;
            let nz = 1.0 - fa.harmonicity;
            self.noise = nz + (self.noise - nz) * a;
        }
        if fa.voiced && fa.n_harm > 0 {
            let norm = power_sum_db(&fa.harm_db[..fa.n_harm]);
            for (k, h) in self.harm_db.iter_mut().enumerate() {
                let x = if k < fa.n_harm { (fa.harm_db[k] - norm).max(FLOOR_DB) } else { FLOOR_DB };
                // A different harmonic count (a pitch change) must not smear
                // a stale level into a slot that has no measurement now.
                *h = if self.harm_valid && k < fa.n_harm { x + (*h - x) * a } else { x };
            }
            self.f0 = fa.f0;
            self.harm_valid = true;
        }
    }
}

/// The parametric Kubyz: 16 harmonic amplitudes (linear) through the formant
/// bank, mixed with the dry additive sum by `vox_mix` — the same structure as
/// `kubyz::voice` (its `tanh` is left out: it is level-dependent, not timbre).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModelTarget {
    pub harmonics: [f32; N_HARMONICS],
    pub bands: FormantBands,
    pub vox_mix: f32,
}

impl ModelTarget {
    /// Mouth response (dB) at `f`: `|(1−mix) + mix·MAKEUP·Σ gᵢ·Hᵢ(f)|`.
    pub fn filter_db(&self, f: f32) -> f32 {
        let (mut re, mut im) = (0.0f32, 0.0f32);
        for i in 0..self.bands.live() {
            let q = effective_q(self.bands.f[i], self.bands.bw[i]);
            let (r, j) = bandpass_response(f, self.bands.f[i], q);
            re += r * self.bands.gain[i];
            im += j * self.bands.gain[i];
        }
        let m = self.vox_mix.clamp(0.0, 1.0);
        let re = (1.0 - m) + m * MAKEUP * re;
        let im = m * MAKEUP * im;
        db_pow(re * re + im * im)
    }

    /// Source level (dB) of harmonic `k` (1-based). Above the model's 16 the
    /// last one rolls off at −18 dB/octave: a jaw harp has no 30th harmonic
    /// to speak of, and a flat extension would read as hiss.
    pub fn source_db(&self, k: usize) -> f32 {
        let k = k.max(1);
        if k <= N_HARMONICS {
            20.0 * self.harmonics[k - 1].max(1e-6).log10()
        } else {
            let last = 20.0 * self.harmonics[N_HARMONICS - 1].max(1e-6).log10();
            last - 18.0 * (k as f32 / N_HARMONICS as f32).log2()
        }
    }

    /// Source level around `x` = f/f0, power-averaged over ±half an octave
    /// of harmonics. The envelope is a smooth curve: sampling the spiky
    /// harmonic table at an arbitrary f0 would put a resonance wherever a
    /// loud harmonic happened to land — a tone painted onto noise.
    fn source_smooth_db(&self, x: f32) -> f32 {
        let x = x.clamp(1.0, MAX_HARM as f32);
        let lo = (x * core::f32::consts::FRAC_1_SQRT_2).ceil().max(1.0) as usize;
        let hi = ((x * core::f32::consts::SQRT_2).floor() as usize).clamp(lo, MAX_HARM);
        let p: f32 = (lo..=hi).map(|k| 10f32.powf(self.source_db(k) * 0.1)).sum();
        db_pow(p / (hi - lo + 1) as f32)
    }
}

/// The target as `morph_gains` consumes it, at the current frame.
#[derive(Clone, Copy, Debug)]
pub struct TargetFrame {
    /// Target level of harmonic k+1 (dB, any common scale).
    pub harm_db: [f32; MAX_HARM],
    /// The target's own envelope at its harmonic k+1 (dB) — divided out by
    /// Vowels so the voice's envelope can take its place.
    pub filt_db: [f32; MAX_HARM],
    pub env_db: [f32; GRID],
    pub harm_ok: bool,
    pub env_ok: bool,
}

impl Default for TargetFrame {
    fn default() -> Self {
        Self {
            harm_db: [FLOOR_DB; MAX_HARM],
            filt_db: [0.0; MAX_HARM],
            env_db: [FLOOR_DB; GRID],
            harm_ok: false,
            env_ok: false,
        }
    }
}

impl TargetFrame {
    /// From a measured profile. Harmonics transfer by number; the target's
    /// envelope at its own harmonics is what Vowels divides out.
    pub fn from_profile(&mut self, p: &TimbreProfile, voiced_now: bool) {
        self.harm_db = p.harm_db;
        for k in 0..MAX_HARM {
            self.filt_db[k] = grid_at(&p.env_db, (k + 1) as f32 * p.f0.max(1.0));
        }
        self.env_db = p.env_db;
        self.harm_ok = p.harm_valid && voiced_now;
        self.env_ok = p.env_valid;
    }

    /// From the Kubyz model played at the voice's pitch `f0`: the mouth
    /// formants stay at fixed frequencies, as on the real instrument.
    pub fn from_model(&mut self, m: &ModelTarget, f0: f32) {
        let f0 = f0.max(30.0);
        for k in 0..MAX_HARM {
            let fk = (k + 1) as f32 * f0;
            let filt = m.filter_db(fk);
            self.filt_db[k] = filt;
            self.harm_db[k] = m.source_db(k + 1) + filt;
        }
        for g in 0..GRID {
            let f = grid_hz(g);
            self.env_db[g] = m.filter_db(f) + m.source_smooth_db(f / f0);
        }
        self.harm_ok = true;
        self.env_ok = true;
    }
}

/// What a frame did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameMode {
    /// Silence or no target: gains 0 dB.
    Pass,
    Harmonic,
    Envelope,
}

#[derive(Clone, Copy, Debug)]
pub struct MorphSettings {
    /// 0 = voice, 1 = target.
    pub morph: f32,
    /// 0 = target's envelope (mouth), 1 = voice's envelope (the words).
    pub vowels: f32,
    /// Residual (breath / noise between harmonics) level at Morph 1, linear.
    pub breath: f32,
    /// Extra tilt at Morph 1, dB per octave around 1 kHz.
    pub tilt_db_oct: f32,
    /// Harmonic mode (falls back to Envelope on unvoiced frames).
    pub harmonic: bool,
}

/// Per-frame scratch for [`morph_gains`], plus what the GUI shows.
pub struct GainScratch {
    grid_gain: [f32; GRID],
    harm_gain: [f32; MAX_HARM],
    /// Voice / target / output harmonic levels of the last Harmonic frame,
    /// each normalised to its own energy (dB) — the display.
    pub show_voice: [f32; MAX_HARM],
    pub show_target: [f32; MAX_HARM],
    pub show_out: [f32; MAX_HARM],
    pub show_n: usize,
}

impl GainScratch {
    pub fn new() -> Self {
        Self {
            grid_gain: [0.0; GRID],
            harm_gain: [0.0; MAX_HARM],
            show_voice: [FLOOR_DB; MAX_HARM],
            show_target: [FLOOR_DB; MAX_HARM],
            show_out: [FLOOR_DB; MAX_HARM],
            show_n: 0,
        }
    }
}

impl Default for GainScratch {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn tilt_db(f: f32, s: &MorphSettings) -> f32 {
    s.morph * s.tilt_db_oct * (f.max(20.0) / 1000.0).log2()
}

/// Moving average over ±COARSE_W grid points.
fn coarse(x: &[f32; GRID], g: usize) -> f32 {
    let lo = g.saturating_sub(COARSE_W);
    let hi = (g + COARSE_W).min(GRID - 1);
    x[lo..=hi].iter().sum::<f32>() / (hi - lo + 1) as f32
}

/// Envelope-mode gain on the grid: voice envelope → target envelope in dB,
/// level-matched first so the clamp only ever bites on shape, never on scale.
fn envelope_gains(voice: &FrameAnalysis, t: &TargetFrame, s: &MorphSettings, sc: &mut GainScratch) {
    if !t.env_ok {
        sc.grid_gain = [0.0; GRID];
        return;
    }
    let floor = voice.level_db - 70.0;
    let mut tgt = [0.0f32; GRID];
    let mut ev = [0.0f32; GRID];
    for g in 0..GRID {
        ev[g] = voice.env_db[g].max(floor);
    }
    for g in 0..GRID {
        // Vowels: swap the target's formant detail (envelope minus its ~1
        // octave trend) for the voice's, keep the target's broad balance.
        let detail = (ev[g] - coarse(&ev, g)) - (t.env_db[g] - coarse(&t.env_db, g));
        tgt[g] = t.env_db[g] + s.vowels * detail;
    }
    let off = power_sum_db(&ev) - power_sum_db(&tgt);
    for g in 0..GRID {
        let d = s.morph * (tgt[g] + off - ev[g]) + tilt_db(grid_hz(g), s);
        sc.grid_gain[g] = d.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
    }
}

/// Harmonic-mode gain per harmonic. Returns false if nothing was measured.
fn harmonic_gains(voice: &FrameAnalysis, t: &TargetFrame, s: &MorphSettings, sc: &mut GainScratch) -> bool {
    let n = voice.n_harm.min(MAX_HARM);
    if n == 0 {
        return false;
    }
    let mut d = [0.0f32; MAX_HARM];
    let (mut num, mut den) = (0.0f32, 0.0f32);
    for k in 0..n {
        let fk = (k + 1) as f32 * voice.f0;
        let ev = grid_at(&voice.env_db, fk);
        let tgt = t.harm_db[k] + s.vowels * (ev - t.filt_db[k]);
        d[k] = tgt - voice.harm_db[k];
        let pv = 10f32.powf(voice.harm_db[k] * 0.1);
        num += pv * 10f32.powf(d[k] * 0.1);
        den += pv;
    }
    // Level-match: the target profile, scaled to the voice's harmonic energy.
    let off = if num > 0.0 && den > 0.0 { -db_pow(num / den) } else { 0.0 };
    for k in 0..n {
        let fk = (k + 1) as f32 * voice.f0;
        let g = s.morph * (d[k] + off) + tilt_db(fk, s);
        sc.harm_gain[k] = g.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
    }
    let vn = power_sum_db(&voice.harm_db[..n]);
    let mut tg = [FLOOR_DB; MAX_HARM];
    let mut og = [FLOOR_DB; MAX_HARM];
    for k in 0..n {
        tg[k] = voice.harm_db[k] + d[k];
        og[k] = voice.harm_db[k] + sc.harm_gain[k];
    }
    let (tn, on) = (power_sum_db(&tg[..n]), power_sum_db(&og[..n]));
    for k in 0..MAX_HARM {
        let live = k < n;
        sc.show_voice[k] = if live { voice.harm_db[k] - vn } else { FLOOR_DB };
        sc.show_target[k] = if live { tg[k] - tn } else { FLOOR_DB };
        sc.show_out[k] = if live { og[k] - on } else { FLOOR_DB };
    }
    sc.show_n = n;
    true
}

/// One gain (dB) per bin into `gain_db` (`half+1`), from the voice frame
/// (`voice` + its power spectrum `pow`), the target and the settings. The
/// result is loudness-matched: Σ pow·gain² = Σ pow, so Morph never moves the
/// level. Morph = 0 gives exactly 0 dB everywhere.
pub fn morph_gains(
    voice: &FrameAnalysis,
    pow: &[f32],
    fpb: f32,
    t: &TargetFrame,
    s: &MorphSettings,
    sc: &mut GainScratch,
    gain_db: &mut [f32],
) -> FrameMode {
    let nb = gain_db.len().min(pow.len());
    if voice.level_db < SILENCE_DB || !(t.env_ok || t.harm_ok) || s.morph <= 0.0 {
        gain_db.fill(0.0);
        return FrameMode::Pass;
    }
    envelope_gains(voice, t, s, sc);
    let harmonic = s.harmonic && voice.voiced && t.harm_ok && harmonic_gains(voice, t, s, sc);
    let breath_db = 20.0 * (1.0 + s.morph * (s.breath - 1.0)).max(1e-3).log10();
    let spacing = voice.f0 / fpb;
    let lobe = (spacing * 0.3).clamp(1.0, 2.0);
    let n = voice.n_harm;
    for b in 0..nb {
        let f = b as f32 * fpb;
        let ge = grid_at(&sc.grid_gain, f);
        if !harmonic {
            gain_db[b] = ge;
            continue;
        }
        let kn = (f / voice.f0).round() as usize;
        let hw = if kn >= 1 && kn <= n {
            let dist = (f - kn as f32 * voice.f0).abs() / fpb;
            (lobe + 1.0 - dist).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let gh = if hw > 0.0 { sc.harm_gain[kn - 1] } else { 0.0 };
        gain_db[b] = hw * gh + (1.0 - hw) * (ge + breath_db);
    }
    // Loudness match over the whole frame (residual included).
    let (mut ein, mut eout) = (0.0f64, 0.0f64);
    for b in 1..nb {
        ein += pow[b] as f64;
        eout += (pow[b] * 10f32.powf(gain_db[b] * 0.1)) as f64;
    }
    if eout > 0.0 && ein > 0.0 {
        let c = db_pow((ein / eout) as f32);
        for g in gain_db[..nb].iter_mut() {
            *g += c;
        }
    }
    if harmonic {
        FrameMode::Harmonic
    } else {
        FrameMode::Envelope
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone_spectrum(n: usize, sr: f32, f0: f32, amps: &[f32]) -> Vec<Complex<f32>> {
        use realfft::RealFftPlanner;
        let mut x: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                let w = 0.5 - 0.5 * (core::f32::consts::TAU * i as f32 / n as f32).cos();
                w * amps
                    .iter()
                    .enumerate()
                    .map(|(k, a)| a * (core::f32::consts::TAU * f0 * (k + 1) as f32 * t).sin())
                    .sum::<f32>()
            })
            .collect();
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
        let mut out = fft.make_output_vec();
        fft.process(&mut x, &mut out).unwrap();
        out
    }

    #[test]
    fn harmonic_levels_and_voicing_of_a_known_tone() {
        let amps: Vec<f32> = (1..=10).map(|k| 0.3 / k as f32).collect();
        let spec = tone_spectrum(2048, 48_000.0, 220.0, &amps);
        let mut an = Analyzer::new(1024, 48_000.0 / 2048.0);
        let mut fa = FrameAnalysis::default();
        an.analyze(&spec, 220.0, &mut fa);
        assert!(fa.voiced, "harmonicity {}", fa.harmonicity);
        for k in 1..10 {
            let want = 20.0 * (amps[k] / amps[0]).log10();
            let got = fa.harm_db[k] - fa.harm_db[0];
            assert!((got - want).abs() < 0.6, "h{} {got} vs {want}", k + 1);
        }
    }

    #[test]
    fn morph_zero_is_unity_and_level_is_kept() {
        let amps: Vec<f32> = (1..=10).map(|k| 0.3 / k as f32).collect();
        let spec = tone_spectrum(2048, 48_000.0, 220.0, &amps);
        let mut an = Analyzer::new(1024, 48_000.0 / 2048.0);
        let mut fa = FrameAnalysis::default();
        an.analyze(&spec, 220.0, &mut fa);
        let m = ModelTarget {
            harmonics: [1.0; N_HARMONICS],
            bands: FormantBands::three([705.0, 1301.0, 2165.0], [200.0, 300.0, 400.0], [1.0, 0.9, 0.75]),
            vox_mix: 1.0,
        };
        let mut t = TargetFrame::default();
        t.from_model(&m, 220.0);
        let mut sc = GainScratch::new();
        let mut g = vec![0.0; 1025];
        let mut s = MorphSettings { morph: 0.0, vowels: 0.0, breath: 0.3, tilt_db_oct: 0.0, harmonic: true };
        assert_eq!(morph_gains(&fa, an.power(), 23.4375, &t, &s, &mut sc, &mut g), FrameMode::Pass);
        assert!(g.iter().all(|&x| x == 0.0));
        s.morph = 1.0;
        assert_eq!(morph_gains(&fa, an.power(), 23.4375, &t, &s, &mut sc, &mut g), FrameMode::Harmonic);
        let ein: f32 = an.power().iter().skip(1).sum();
        let eout: f32 = an.power().iter().zip(&g).skip(1).map(|(p, d)| p * 10f32.powf(d / 10.0)).sum();
        assert!((10.0 * (eout / ein).log10()).abs() < 0.01);
    }
}
