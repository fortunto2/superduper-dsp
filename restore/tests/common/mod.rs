//! Signals and measures shared by the restore tests (and the denoise plugin's quality test,
//! which includes this file by path), so "the same talker" is the same everywhere.
#![allow(dead_code)]

pub const SR: f32 = 48_000.0;

/// xorshift32 → uniform in [-1, 1).
pub struct Rng(pub u32);
impl Rng {
    pub fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

pub fn white(n: usize, amp: f32, seed: u32) -> Vec<f32> {
    let mut r = Rng(seed);
    (0..n).map(|_| r.next() * amp).collect()
}

pub fn tone(n: usize, f: f32, amp: f32) -> Vec<f32> {
    (0..n).map(|i| amp * (std::f32::consts::TAU * f * i as f32 / SR).sin()).collect()
}

pub fn energy(x: &[f32]) -> f64 {
    x.iter().map(|&v| v as f64 * v as f64).sum()
}

pub fn db(x: f64) -> f64 {
    10.0 * x.max(1e-30).log10()
}

/// SNR of `y` against the known clean `s` over `range` (residual = y − s).
pub fn snr_db(s: &[f32], y: &[f32], range: std::ops::Range<usize>) -> f64 {
    let sig = energy(&s[range.clone()]);
    let res: f64 = range.map(|i| (y[i] as f64 - s[i] as f64).powi(2)).sum();
    db(sig / res)
}

/// A voiced synthetic talker: harmonic stack on a gliding f0 (110..170 Hz), each harmonic
/// weighted by three formant bumps (700 / 1220 / 2600 Hz), syllables at ~4 Hz with gaps.
/// Not speech, but it has what RNNoise keys on: pitch, a spectral envelope, onsets.
pub fn voiced(n: usize) -> Vec<f32> {
    let formant = |f: f32| {
        let bump = |c: f32, w: f32| (-((f - c) / w).powi(2)).exp();
        bump(700.0, 180.0) + 0.6 * bump(1220.0, 220.0) + 0.3 * bump(2600.0, 350.0) + 0.02
    };
    let mut phase = 0.0f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let f0 = 140.0 + 30.0 * (std::f32::consts::TAU * 0.7 * t).sin();
            phase += std::f32::consts::TAU * f0 / SR;
            let syl = (std::f32::consts::TAU * 4.0 * t).sin().max(0.0).powf(0.7);
            let mut s = 0.0;
            for h in 1..40 {
                let f = f0 * h as f32;
                if f > 8000.0 {
                    break;
                }
                s += formant(f) * (phase * h as f32).sin();
            }
            0.08 * s * syl
        })
        .collect()
}
