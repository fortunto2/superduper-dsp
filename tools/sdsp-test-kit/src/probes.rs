//! The standard probes. Each returns one number, so it can go in a snapshot.
//!
//! The set is chosen from faults this project actually shipped: a kit with no
//! energy above 2.5 kHz (band_energy), a meter reading 3 dB light (gain_db), a
//! voice stealing click (max_step), an oversampler regression (aliasing), and a
//! plugin that quietly stopped doing anything (gain_db again, or thd).

use superduper_synth_core::analysis::{
    magnitude_spectrum_db, make_bin_aligned_sine, measure_aliasing_db, measure_thd_db,
};

/// Bands worth watching separately, matching tools/mixcheck.py.
pub const BANDS: &[(f32, f32, &str)] = &[
    (20.0, 60.0, "sub"),
    (60.0, 120.0, "kick"),
    (120.0, 250.0, "low"),
    (250.0, 800.0, "body"),
    (800.0, 2500.0, "mid"),
    (2500.0, 6000.0, "presence"),
    (6000.0, 16000.0, "air"),
];

pub fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

pub fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0_f32, |m, v| m.max(v.abs()))
}

pub fn db(x: f32) -> f64 {
    20.0 * (x.abs().max(1e-12) as f64).log10()
}

/// Output level relative to input — catches "the plugin went quiet" and
/// "the plugin got 3 dB louder", which is most of what silently changes.
pub fn gain_db(input: &[f32], output: &[f32]) -> f64 {
    db(rms(output)) - db(rms(input))
}

/// Largest sample-to-sample jump. A click shows up here and nowhere else;
/// lesson 19 in the project's CLAUDE.md exists because of it.
pub fn max_step(x: &[f32]) -> f64 {
    x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0_f32, f32::max) as f64
}

/// Energy per band, in dB relative to the loudest band. Shape, not level —
/// so it stays meaningful when the plugin's output gain changes.
pub fn band_shape(x: &[f32], sr: f32) -> Vec<(&'static str, f64)> {
    let n = 4096.min(x.len().next_power_of_two() / 2).max(1024);
    let mut acc = vec![0.0_f64; BANDS.len()];
    let mut frames = 0;
    let mut pos = 0;
    while pos + n <= x.len() {
        let spec = magnitude_spectrum_db(&x[pos..pos + n]);
        for (bi, (lo, hi, _)) in BANDS.iter().enumerate() {
            let i0 = (*lo * n as f32 / sr) as usize;
            let i1 = ((*hi * n as f32 / sr) as usize).min(spec.len().saturating_sub(1));
            let mut sum = 0.0;
            for v in &spec[i0..=i1.max(i0)] {
                // spectrum comes back in dB; convert to power to average
                sum += 10f64.powf(*v as f64 / 10.0);
            }
            acc[bi] += sum;
        }
        frames += 1;
        pos += n;
    }
    if frames == 0 {
        return BANDS.iter().map(|(_, _, n)| (*n, -120.0)).collect();
    }
    let loudest = acc.iter().cloned().fold(f64::MIN, f64::max).max(1e-30);
    BANDS
        .iter()
        .enumerate()
        .map(|(i, (_, _, name))| (*name, 10.0 * (acc[i] / loudest).log10()))
        .collect()
}

/// Harmonic distortion at 1 kHz — the number that says "this saturator still
/// saturates" and "this EQ still doesn't".
pub fn thd_at(x: &[f32], f0: f32, sr: f32) -> f64 {
    measure_thd_db(x, f0, sr) as f64
}

/// Worst non-harmonic peak; regressions in oversampling land here.
pub fn aliasing_at(x: &[f32], f0: f32, sr: f32) -> f64 {
    measure_aliasing_db(x, f0, sr) as f64
}

/// A test tone that lands exactly on an FFT bin, so THD isn't polluted by leakage.
pub fn tone(sr: f32, hz: f32, amp: f32, frames: usize) -> Vec<f32> {
    let n = frames.next_power_of_two().min(1 << 16);
    // make_bin_aligned_sine returns (samples, the frequency it snapped to).
    let (mut v, _snapped) = make_bin_aligned_sine(n, sr, hz, amp);
    v.resize(frames, 0.0);
    v
}

/// White-ish noise with a fixed seed — deterministic, unlike the plugins' own
/// generators, so the same probe gives the same number every run.
pub fn noise(frames: usize, amp: f32) -> Vec<f32> {
    let mut state = 0x1234_5678_u32;
    (0..frames)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            ((state as f32 / u32::MAX as f32) * 2.0 - 1.0) * amp
        })
        .collect()
}

/// An impulse followed by silence — for tails, latency and click behaviour.
pub fn impulse(frames: usize) -> Vec<f32> {
    let mut v = vec![0.0; frames];
    if let Some(first) = v.first_mut() {
        *first = 1.0;
    }
    v
}
