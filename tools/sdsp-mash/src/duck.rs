//! Sidechain ducker — a vocal-keyed compressor gain computer.
//!
//! The vocal is the *key*; its envelope drives gain reduction on a *target*
//! bus (`beat-other`). This mirrors ffmpeg's `sidechaincompress`: a peak
//! follower with asymmetric attack/release tracks the key level, a static
//! soft-knee compressor curve turns that level into gain reduction.
//!
//! The gain computer is a pure per-sample function of the key — testable in
//! isolation, no audio buffers, no allocation.

use superduper_synth_core::dsp_blocks::compressor_gain_db;

/// Ducking parameters (already unit-resolved from the TOML config).
#[derive(Debug, Clone, Copy)]
pub struct DuckParams {
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
}

/// Linear amplitude → dBFS, floored so silence maps to a finite value.
#[inline]
pub fn lin_to_db(x: f32) -> f32 {
    20.0 * x.abs().max(1e-9).log10()
}

/// dB → linear amplitude.
#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Stateful envelope follower + gain computer. One instance per rendered
/// target bus; `gain_for` is called once per sample with the key value.
#[derive(Debug, Default, Clone, Copy)]
pub struct Ducker {
    env: f32,
}

impl Ducker {
    pub fn new() -> Self {
        Self { env: 0.0 }
    }

    /// Advance the follower by one key sample and return the linear gain
    /// (0, 1] to apply to the target this sample. `key` should be the mono
    /// key level (e.g. `max(|L|, |R|)`).
    #[inline]
    pub fn gain_for(&mut self, key: f32, sr: f32, p: &DuckParams) -> f32 {
        let rect = key.abs();
        // Asymmetric one-pole: fast to rise (attack) when the key gets
        // louder, slow to fall (release) when it quietens.
        let tc_ms = if rect > self.env {
            p.attack_ms
        } else {
            p.release_ms
        };
        let coef = (-1.0 / (tc_ms.max(0.01) * 0.001 * sr)).exp();
        self.env = rect + (self.env - rect) * coef;

        // Static soft-knee curve → gain reduction (≤ 0 dB), then to linear.
        let key_db = lin_to_db(self.env);
        let gr_db = compressor_gain_db(key_db, p.threshold_db, p.ratio, p.knee_db);
        db_to_lin(gr_db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 44_100.0;

    fn params() -> DuckParams {
        DuckParams {
            threshold_db: -30.0,
            ratio: 4.0,
            attack_ms: 5.0,
            release_ms: 120.0,
            knee_db: 6.0,
        }
    }

    #[test]
    fn silence_key_gives_unity_gain() {
        let mut d = Ducker::new();
        let p = params();
        let mut g = 1.0;
        for _ in 0..1000 {
            g = d.gain_for(0.0, SR, &p);
        }
        assert!((g - 1.0).abs() < 1e-6, "silence must not duck, got {g}");
    }

    #[test]
    fn loud_key_reaches_expected_steady_state() {
        // A sustained key at 0 dBFS (|x| = 1.0) → env → 1.0 → 0 dB.
        // Steady-state gain reduction from the static curve at 0 dB in:
        //   gr = compressor_gain_db(0, -30, 4, 6)
        let p = params();
        let expected_gr = compressor_gain_db(0.0, p.threshold_db, p.ratio, p.knee_db);
        let expected = db_to_lin(expected_gr);

        let mut d = Ducker::new();
        let mut g = 1.0;
        // Hold the key high long past the attack time constant.
        for _ in 0..(SR as usize) {
            g = d.gain_for(1.0, SR, &p);
        }
        assert!(
            (g - expected).abs() < 1e-3,
            "steady gain {g} != expected {expected}"
        );
        // And it must actually be reducing gain meaningfully.
        assert!(g < 0.5, "loud vocal should duck hard, got {g}");
    }

    #[test]
    fn below_threshold_key_barely_ducks() {
        // Key at -40 dBFS is below the -30 dB threshold (minus half-knee),
        // so gain reduction should be ~0 → gain ~1.0.
        let p = params();
        let key = db_to_lin(-40.0);
        let mut d = Ducker::new();
        let mut g = 1.0;
        for _ in 0..(SR as usize) {
            g = d.gain_for(key, SR, &p);
        }
        assert!(g > 0.99, "quiet key should not duck, got {g}");
    }

    #[test]
    fn attack_is_gradual_not_instant() {
        // On a step from silence to full key, the very first sample must not
        // already sit at the steady-state gain — the follower ramps in.
        let p = params();
        let mut d = Ducker::new();
        let g_first = d.gain_for(1.0, SR, &p);
        // First sample: env is still tiny → almost no reduction yet.
        assert!(g_first > 0.9, "attack should be gradual, got {g_first}");

        // After ~1 attack time constant (5 ms ≈ 220 samples) it should have
        // moved a long way toward steady state.
        for _ in 0..441 {
            d.gain_for(1.0, SR, &p);
        }
        let g_later = d.gain_for(1.0, SR, &p);
        assert!(
            g_later < g_first - 0.2,
            "gain should fall during attack: first {g_first}, later {g_later}"
        );
    }

    #[test]
    fn release_recovers_after_key_stops() {
        let p = params();
        let mut d = Ducker::new();
        // Duck hard.
        for _ in 0..(SR as usize / 2) {
            d.gain_for(1.0, SR, &p);
        }
        let g_ducked = d.gain_for(1.0, SR, &p);
        // Key stops; release lets gain climb back toward unity.
        let mut g = g_ducked;
        for _ in 0..(SR as usize) {
            g = d.gain_for(0.0, SR, &p);
        }
        assert!(
            g > g_ducked + 0.3,
            "release should recover gain: ducked {g_ducked}, after {g}"
        );
    }

    #[test]
    fn db_helpers_round_trip() {
        assert!((db_to_lin(0.0) - 1.0).abs() < 1e-9);
        assert!((lin_to_db(1.0)).abs() < 1e-6);
        assert!((db_to_lin(-6.0) - 0.5011872).abs() < 1e-5);
    }
}
