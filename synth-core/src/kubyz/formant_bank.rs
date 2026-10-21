//! Variable-count formant bank for Kubyz — 1 to `MAX_FORMANTS` parallel
//! band-passes, so the GUI's formant editor can add and remove resonances.
//!
//! Why not grow `crate::formant::Formant`: that 3-band block is shared by
//! Wind, Formant, Tune, Vocoder and the iOS engine, and its `[f32; 3]` API is
//! what they all call. This bank is the same filter generalised — identical
//! biquad, Q clamp, make-up gain and `tanh` — and with `count == 3` it
//! produces bit-identical output (pinned by a test below), so old Kubyz
//! projects and the 3-band factory presets sound exactly as before.
//!
//! Real-time: fixed-size arrays only. Bands at index ≥ `count` cost nothing —
//! not tuned, not processed — and their filter state is cleared the moment
//! they drop out, so a band added back later starts from silence instead of
//! replaying a stale tail.

use crate::dsp_blocks::Biquad;

/// Upper bound on formants. Six covers F1–F5 plus a "singer's formant" /
/// nasal pole; past that a parallel band-pass bank stops reading as a vowel.
pub const MAX_FORMANTS: usize = 6;

/// Make-up gain — same value and reasoning as `formant::MAKEUP` (band-pass
/// sum sits ~10 dB under a unity sine at a formant centre).
pub const MAKEUP: f32 = 1.8;

/// Lowest / highest Q the bank will tune. Above ~12 a band-pass rings
/// forever on a sustained drone; below 0.5 it is no longer a resonance.
pub const Q_MIN: f32 = 0.5;
pub const Q_MAX: f32 = 12.0;

/// The bandwidth the filter actually uses for `bw` at centre `f` — the
/// stored value goes through the same Q clamp as `ensure_tuned`. The GUI
/// draws with this, so a handle sits where the sound is, not where a
/// clamped-away number says.
#[inline]
pub fn effective_q(f: f32, bw: f32) -> f32 {
    (f / bw.max(20.0)).clamp(Q_MIN, Q_MAX)
}

/// One full formant set: centre (Hz), bandwidth (Hz), linear gain per band,
/// and how many leading bands are live. Copy, fixed-size — safe to build on
/// the audio thread per sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FormantBands {
    pub f: [f32; MAX_FORMANTS],
    pub bw: [f32; MAX_FORMANTS],
    pub gain: [f32; MAX_FORMANTS],
    /// Live bands, `1..=MAX_FORMANTS` (0 is treated as dry-through).
    pub count: usize,
}

impl FormantBands {
    /// The classic 3-band set — what every Kubyz caller had before the bank
    /// grew. Unused slots hold harmless defaults.
    pub fn three(f: [f32; 3], bw: [f32; 3], gain: [f32; 3]) -> Self {
        let mut out = Self {
            f: [1000.0; MAX_FORMANTS],
            bw: [200.0; MAX_FORMANTS],
            gain: [0.0; MAX_FORMANTS],
            count: 3,
        };
        out.f[..3].copy_from_slice(&f);
        out.bw[..3].copy_from_slice(&bw);
        out.gain[..3].copy_from_slice(&gain);
        out
    }

    /// Live band count, clamped to the array.
    #[inline]
    pub fn live(&self) -> usize {
        self.count.min(MAX_FORMANTS)
    }
}

/// Stereo-paired bank of up to `MAX_FORMANTS` band-passes.
#[derive(Default, Clone, Copy)]
pub struct FormantBank {
    bp_l: [Biquad; MAX_FORMANTS],
    bp_r: [Biquad; MAX_FORMANTS],
    cached: Cached,
}

#[derive(Default, Clone, Copy, PartialEq)]
struct Cached {
    f: [f32; MAX_FORMANTS],
    bw: [f32; MAX_FORMANTS],
    count: usize,
    sr: f32,
}

impl FormantBank {
    /// Recompute coefficients only when the tuning of a live band changed.
    /// Bands that just went dead get their state cleared here (once).
    fn ensure_tuned(&mut self, sr: f32, b: &FormantBands) {
        let n = b.live();
        let mut next = Cached { f: [0.0; MAX_FORMANTS], bw: [0.0; MAX_FORMANTS], count: n, sr };
        next.f[..n].copy_from_slice(&b.f[..n]);
        next.bw[..n].copy_from_slice(&b.bw[..n]);
        if self.cached == next {
            return;
        }
        for i in 0..n {
            let q = effective_q(b.f[i], b.bw[i]);
            self.bp_l[i].set_bandpass(sr, b.f[i], q);
            self.bp_r[i].set_bandpass(sr, b.f[i], q);
        }
        for i in n..self.cached.count.min(MAX_FORMANTS) {
            self.bp_l[i].clear();
            self.bp_r[i].clear();
        }
        self.cached = next;
    }

    /// Process one stereo pair. `mix` ∈ [0,1] — 0 = dry, 1 = full formant.
    /// Same arithmetic, in the same order, as `formant::Formant::process`.
    #[inline]
    pub fn process(&mut self, l: f32, r: f32, sr: f32, b: &FormantBands, mix: f32) -> (f32, f32) {
        if mix <= 0.0 {
            return (l, r);
        }
        self.ensure_tuned(sr, b);
        let mut wet_l = 0.0_f32;
        let mut wet_r = 0.0_f32;
        for i in 0..b.live() {
            wet_l += self.bp_l[i].process(l) * b.gain[i];
            wet_r += self.bp_r[i].process(r) * b.gain[i];
        }
        let mix = mix.clamp(0.0, 1.0);
        let out_l = l * (1.0 - mix) + (wet_l * MAKEUP).tanh() * mix;
        let out_r = r * (1.0 - mix) + (wet_r * MAKEUP).tanh() * mix;
        (out_l, out_r)
    }
}

/// Complex response of one band-pass (analog RBJ prototype, 0 dB peak) at
/// `freq` — `(re, im)`. Parallel bands sum as complex numbers, which is why
/// the GUI's curve uses this instead of adding magnitudes.
#[inline]
pub fn bandpass_response(freq: f32, centre: f32, q: f32) -> (f32, f32) {
    // H(jx) = (j x/Q) / (1 - x² + j x/Q), x = f / fc.
    let x = freq / centre.max(1.0);
    let a = 1.0 - x * x;
    let b = x / q;
    let den = a * a + b * b;
    if den <= 1e-20 {
        return (1.0, 0.0);
    }
    // (j b)(a - j b) / den = (b² + j a b) / den
    (b * b / den, a * b / den)
}

/// Magnitude (linear, pre-`tanh`, including make-up) of the whole live bank
/// at `freq`. GUI-side helper for drawing the summed curve.
pub fn bank_magnitude(b: &FormantBands, freq: f32) -> f32 {
    let (mut re, mut im) = (0.0_f32, 0.0_f32);
    for i in 0..b.live() {
        let (r, j) = bandpass_response(freq, b.f[i], effective_q(b.f[i], b.bw[i]));
        re += r * b.gain[i];
        im += j * b.gain[i];
    }
    (re * re + im * im).sqrt() * MAKEUP
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formant::Formant;

    #[test]
    fn three_bands_match_the_shared_formant_bit_for_bit() {
        let f = [705.0, 1301.0, 2165.0];
        let bw = [200.0, 300.0, 400.0];
        let g = [1.0, 0.9, 0.75];
        let bands = FormantBands::three(f, bw, g);
        let mut old = Formant::default();
        let mut new = FormantBank::default();
        let mut phase = 0.0_f32;
        for _ in 0..4096 {
            phase = (phase + 110.0 / 48_000.0).fract();
            let x = (phase * core::f32::consts::TAU).sin() * 0.7;
            let a = old.process(x, x * 0.9, 48_000.0, f, bw, g, 0.8);
            let b = new.process(x, x * 0.9, 48_000.0, &bands, 0.8);
            assert_eq!(a, b);
        }
    }

    #[test]
    fn dead_bands_cost_nothing_and_restart_clean() {
        let mut bands = FormantBands::three([500.0, 1500.0, 2500.0], [100.0; 3], [1.0; 3]);
        bands.count = 5;
        bands.f[3] = 3500.0;
        bands.f[4] = 4500.0;
        bands.gain[3] = 1.0;
        bands.gain[4] = 1.0;
        let mut bank = FormantBank::default();
        for i in 0..256 {
            bank.process((i as f32 * 0.3).sin(), 0.0, 48_000.0, &bands, 1.0);
        }
        bands.count = 3;
        bank.process(0.0, 0.0, 48_000.0, &bands, 1.0);
        // Band 4 (index 3) was cleared when it dropped out: re-enabled with
        // silent input it outputs exactly zero, not a stale tail.
        bands.count = 4;
        bands.gain = [0.0, 0.0, 0.0, 1.0, 0.0, 0.0];
        let (l, _) = bank.process(0.0, 0.0, 48_000.0, &bands, 1.0);
        assert_eq!(l, 0.0);
    }

    #[test]
    fn response_peaks_at_unity_on_centre() {
        let (re, im) = bandpass_response(1000.0, 1000.0, 5.0);
        assert!((re - 1.0).abs() < 1e-5 && im.abs() < 1e-5);
        // -3 dB points: |H| = 1/√2 at f_hi - f_lo = fc / Q.
        let bw = 200.0_f32;
        let hi = bw / 2.0 + (bw * bw / 4.0 + 1000.0 * 1000.0).sqrt();
        let (re, im) = bandpass_response(hi, 1000.0, 5.0);
        assert!(((re * re + im * im).sqrt() - core::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
    }
}
