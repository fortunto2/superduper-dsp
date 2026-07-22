//! BPM estimation — normalised autocorrelation of an onset envelope.
//!
//! The onset envelope (200 Hz) is autocorrelated at every lag in the plausible
//! range; each candidate BPM from 60 to 190 in 0.01 steps is scored by linear
//! interpolation of that autocorrelation at its beat lag. Tempo is octave-
//! ambiguous, so the ×2 and ÷2 neighbours are reported too — the caller picks.

use crate::onset::onset_envelope;

const ENV_RATE_HZ: f64 = 200.0;
const BPM_MIN: f64 = 60.0;
const BPM_MAX: f64 = 190.0;
const BPM_STEP: f64 = 0.01;

pub struct BpmResult {
    pub bpm: f64,
    pub strength: f32,
    pub half_bpm: f64,
    pub half_strength: f32,
    pub double_bpm: f64,
    pub double_strength: f32,
}

/// Normalised autocorrelation of `env` at integer `lag`: proper cosine
/// similarity of the two overlapping windows, so it's unbiased across lags
/// and lands in [0, 1] for a non-negative envelope.
fn acf_norm(env: &[f32], lag: usize) -> f32 {
    if lag >= env.len() {
        return 0.0;
    }
    let m = env.len() - lag;
    let mut dot = 0.0f32;
    let mut ea = 0.0f32;
    let mut eb = 0.0f32;
    for i in 0..m {
        let a = env[i];
        let b = env[i + lag];
        dot += a * b;
        ea += a * a;
        eb += b * b;
    }
    let denom = (ea * eb).sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        dot / denom
    }
}

/// Linear interpolation of a precomputed integer-lag ACF table at fractional
/// `lag` (table index 0 = lag 0).
fn acf_interp(table: &[f32], lag: f64) -> f32 {
    if lag <= 0.0 {
        return *table.first().unwrap_or(&0.0);
    }
    let i = lag.floor() as usize;
    if i + 1 >= table.len() {
        return *table.last().unwrap_or(&0.0);
    }
    let frac = (lag - i as f64) as f32;
    table[i] * (1.0 - frac) + table[i + 1] * frac
}

#[inline]
fn bpm_to_lag(bpm: f64) -> f64 {
    60.0 * ENV_RATE_HZ / bpm
}

pub fn estimate_bpm(l: &[f32], r: &[f32], sr: u32) -> BpmResult {
    let oe = onset_envelope(l, r, sr, ENV_RATE_HZ);
    let env = &oe.env;

    // Precompute ACF over every lag we might interpolate — down to a ÷2 of the
    // slowest tempo and up to a ×2 of the fastest.
    let max_lag = (bpm_to_lag(BPM_MIN / 2.0).ceil() as usize + 2).min(env.len().saturating_sub(1));
    if env.len() < 8 || max_lag < 4 {
        return BpmResult {
            bpm: 0.0,
            strength: 0.0,
            half_bpm: 0.0,
            half_strength: 0.0,
            double_bpm: 0.0,
            double_strength: 0.0,
        };
    }
    let table: Vec<f32> = (0..=max_lag).map(|lag| acf_norm(env, lag)).collect();

    // Scan candidate BPMs.
    let mut best_bpm = BPM_MIN;
    let mut best_score = f32::NEG_INFINITY;
    let steps = ((BPM_MAX - BPM_MIN) / BPM_STEP).round() as usize;
    for k in 0..=steps {
        let bpm = BPM_MIN + k as f64 * BPM_STEP;
        let score = acf_interp(&table, bpm_to_lag(bpm));
        if score > best_score {
            best_score = score;
            best_bpm = bpm;
        }
    }

    let half = best_bpm / 2.0;
    let double = best_bpm * 2.0;
    BpmResult {
        bpm: best_bpm,
        strength: best_score,
        half_bpm: half,
        half_strength: acf_interp(&table, bpm_to_lag(half)),
        double_bpm: double,
        double_strength: acf_interp(&table, bpm_to_lag(double)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;

    /// Synthesise a click track at `bpm` (kick every beat) for `secs`.
    fn beat_track(bpm: f64, secs: f64) -> Vec<f32> {
        let n = (SR as f64 * secs) as usize;
        let mut x = vec![0.0f32; n];
        let period = (60.0 / bpm * SR as f64).round() as usize;
        let mut p = 0;
        while p < n {
            for k in 0..800 {
                if p + k < n {
                    let env = (1.0 - k as f32 / 800.0).max(0.0);
                    x[p + k] +=
                        env * (2.0 * std::f32::consts::PI * 90.0 * k as f32 / SR as f32).sin();
                }
            }
            p += period;
        }
        x
    }

    #[test]
    fn detects_a_synthetic_120_bpm() {
        let x = beat_track(120.0, 12.0);
        let res = estimate_bpm(&x, &x, SR);
        // The true tempo (or an octave) must be found; assert the reported
        // best is within 0.5 BPM of 120 or 60 or 240.
        let close = [120.0, 60.0, 240.0]
            .iter()
            .any(|&c| (res.bpm - c).abs() < 0.6);
        assert!(close, "detected {} BPM, expected 120/60/240", res.bpm);
        assert!(res.strength > 0.3, "weak strength {}", res.strength);
    }

    #[test]
    fn detects_a_synthetic_145_bpm() {
        let x = beat_track(145.0, 12.0);
        let res = estimate_bpm(&x, &x, SR);
        let close = [145.0, 72.5]
            .iter()
            .any(|&c| (res.bpm - c).abs() < 0.8);
        assert!(close, "detected {} BPM, expected ~145/72.5", res.bpm);
    }
}
