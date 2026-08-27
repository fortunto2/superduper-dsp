//! Reference signals for every test that has to answer "does this material
//! suit PSOLA?".
//!
//! These mirror the generators in
//! `effects/superduper-pitch/tests/engine_transparency.rs`, which is where
//! the noise-to-harmonic baseline table lives. **Keep them identical** — the
//! numbers in `epoch_sharpness`'s doc comment pair a sharpness value from
//! here with a dB value from there, and they only mean something together.
//! synth-core cannot depend on a plugin crate, so a mirror is the price.

#![allow(dead_code)]

use superduper_synth_core::dsp_blocks::{Biquad, Xorshift};

pub const SR: f32 = 48_000.0;
pub const F0: f32 = 220.0;

/// Smooth harmonic tone — a synth, a sustained kubyz, any sum-of-sines.
/// No glottal pulse anywhere in it, so nothing for PSOLA to snap to.
pub fn smooth(secs: f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            (1..=6)
                .map(|k| (core::f32::consts::TAU * F0 * k as f32 * t).sin() / k as f32)
                .sum::<f32>()
                * 0.3
        })
        .collect()
}

/// Impulse train through two formant resonators — one sharp, unambiguous
/// epoch per period. The material PSOLA was designed for.
pub fn pulsed(secs: f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    let period = SR / F0;
    let mut b1 = Biquad::default();
    b1.set_bandpass(SR, 700.0, 5.0);
    let mut b2 = Biquad::default();
    b2.set_bandpass(SR, 1800.0, 8.0);
    let mut ph = 0.0f32;
    (0..n)
        .map(|_| {
            ph += 1.0;
            let src = if ph >= period {
                ph -= period;
                1.0
            } else {
                0.0
            };
            (b1.process(src) + 0.5 * b2.process(src)) * 0.6
        })
        .collect()
}

/// Rosenberg glottal-flow pulses with jitter + shimmer through three
/// formants, plus `breath` of aspiration noise. `voiced(2.0, 0.02, 0.4)` is a
/// normal sung note; `voiced(2.0, 0.5, 0.62)` is a breathy take — the epoch
/// is still there but the long open quotient blunts the closure, which is
/// what makes it behave like a smooth tone under PSOLA.
pub fn voiced(secs: f32, breath: f32, open: f32) -> Vec<f32> {
    voiced_at(secs, F0, breath, open)
}

/// Same, at an arbitrary fundamental — a bass sitting under the engine's old
/// 95 Hz floor is the case that never locked.
pub fn voiced_at(secs: f32, f0: f32, breath: f32, open: f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    let mut rng = Xorshift::new(0x5EED_1234);
    let mut bank: Vec<Biquad> = [(730.0, 9.0), (1090.0, 11.0), (2440.0, 13.0)]
        .iter()
        .map(|&(hz, q)| {
            let mut b = Biquad::default();
            b.set_bandpass(SR, hz, q);
            b
        })
        .collect();
    let mut air = Biquad::default();
    air.set_bandpass(SR, 2600.0, 0.8);

    let mut ph = 0.0f32;
    let mut period = SR / f0;
    let mut amp = 1.0f32;
    let mut prev_g = 0.0f32;
    (0..n)
        .map(|_| {
            ph += 1.0;
            if ph >= period {
                ph -= period;
                period = SR / f0 * (1.0 + 0.004 * rng.next_bipolar());
                amp = 1.0 + 0.06 * rng.next_bipolar();
            }
            let t = ph / period;
            let (t1, t2) = (open, open * 0.4);
            let g = amp
                * if t < t1 {
                    0.5 * (1.0 - (core::f32::consts::PI * t / t1).cos())
                } else if t < t1 + t2 {
                    (core::f32::consts::FRAC_PI_2 * (t - t1) / t2).cos()
                } else {
                    0.0
                };
            let exc = (g - prev_g) * 40.0;
            prev_g = g;
            let noise = air.process(rng.next_bipolar()) * breath;
            let out: f32 = bank.iter_mut().map(|b| b.process(exc + noise)).sum();
            out * 0.5
        })
        .collect()
}

/// Noise energy (everything off the harmonic grid) relative to harmonic
/// energy, in dB. Lower is cleaner. Mirrors the metric in
/// `superduper-pitch/tests/engine_transparency.rs`; `skip` lets a caller step
/// past an engine's latency and any routing settling time.
///
/// Assumes ONE stationary f0 — on a moving pitch every harmonic lands off the
/// grid and the number stops meaning anything.
pub fn noise_to_harmonic(x: &[f32], f0: f32, skip: f32) -> f32 {
    let start = (skip * SR) as usize;
    let len = (1.0 * SR) as usize;
    let seg: Vec<f32> = x[start..start + len]
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let w = 0.5 - 0.5 * (core::f32::consts::TAU * i as f32 / len as f32).cos();
            v * w
        })
        .collect();
    let spec = superduper_synth_core::analysis::magnitude_spectrum_db(&seg);
    let bin_hz = SR / len as f32;
    let (mut harm, mut noise) = (0.0f64, 0.0f64);
    for (i, &db) in spec.iter().enumerate() {
        let hz = i as f32 * bin_hz;
        if hz < 60.0 || hz > 8000.0 {
            continue;
        }
        let lin = 10f64.powf(db as f64 / 20.0);
        let e = lin * lin;
        if (1..=36).any(|k| (hz - f0 * k as f32).abs() < 7.0 * bin_hz) {
            harm += e;
        } else {
            noise += e;
        }
    }
    10.0 * ((noise / harm.max(1e-30)) as f32).log10()
}

/// RMS of `x[a..b]` in dB.
pub fn rms_db(x: &[f32], a: usize, b: usize) -> f32 {
    let n = (b.min(x.len())).saturating_sub(a).max(1);
    let e: f32 = x[a..a + n].iter().map(|v| v * v).sum();
    10.0 * (e / n as f32).max(1e-30).log10()
}

/// Largest sample-to-sample step in `x[a..b]` — the click detector from
/// lesson 19.
pub fn max_step(x: &[f32], a: usize, b: usize) -> f32 {
    let b = b.min(x.len());
    x[a..b].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max)
}
