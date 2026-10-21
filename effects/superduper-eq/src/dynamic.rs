//! Dynamic Mid band: the bell's gain moves from its static setting toward
//! static + Dyn Gain as a detector level climbs from the floor to floor + Range.
//!
//! The detector is either the bus key track's level at the Mid frequency
//! (Pro-Q's "external side-chain" without any DAW routing) or the band's own
//! input level (classic dynamic EQ). Control runs per `CHUNK` samples: the
//! bell coefficients are recomputed that often, never per sample.

use superduper_synth_core::dsp_blocks::Biquad;

/// The detector level where the move starts: the bus's "loud" line, same as
/// Soothe's key duck.
pub const FLOOR_DB: f32 = -50.0;
/// Control period in samples (0.67 ms at 48 kHz).
pub const CHUNK: usize = 32;
pub const ATTACK_MS: f32 = 10.0;
pub const RELEASE_MS: f32 = 120.0;
/// Self-detector's mean-square smoothing.
const DETECT_MS: f32 = 5.0;

/// Gain offset (dB, ≤ 0) for a detector level: 0 at or below the floor,
/// `dyn_gain` at floor + range and above.
pub fn target_db(level_db: f32, dyn_gain: f32, range: f32) -> f32 {
    dyn_gain * ((level_db - FLOOR_DB) / range.max(1.0)).clamp(0.0, 1.0)
}

fn one_pole(n: usize, ms: f32, sr: f32) -> f32 {
    1.0 - (-(n as f32) / (ms * 0.001 * sr).max(1.0)).exp()
}

#[derive(Default)]
pub struct DynBand {
    /// Current smoothed gain offset added to the static Mid gain (dB).
    pub offset_db: f32,
    det: Biquad,
    ms: f32,
}

impl DynBand {
    /// Retune the self-detector to the band (once per block).
    pub fn tune(&mut self, sr: f32, freq: f32, q: f32) {
        self.det.set_bandpass(sr, freq, q);
    }

    /// Self mode: feed one mono input sample to the band-limited detector.
    #[inline]
    pub fn detect(&mut self, x: f32, coef: f32) {
        let y = self.det.process(x);
        self.ms += (y * y - self.ms) * coef;
    }

    pub fn detect_coef(sr: f32) -> f32 { one_pole(1, DETECT_MS, sr) }

    /// Self-detector level, dB RMS (the bus's band levels are RMS too).
    pub fn self_level_db(&self) -> f32 { 10.0 * (self.ms + 1e-12).log10() }

    /// Move the offset toward `target` over `n` samples: attack when the cut
    /// deepens, release when it lets go. Snaps to exactly 0 once negligible,
    /// so the plugin can fall back to the static path.
    pub fn step(&mut self, target: f32, n: usize, sr: f32) -> f32 {
        let ms = if target < self.offset_db { ATTACK_MS } else { RELEASE_MS };
        self.offset_db += (target - self.offset_db) * one_pole(n, ms, sr);
        if target == 0.0 && self.offset_db.abs() < 1e-3 {
            self.offset_db = 0.0;
        }
        self.offset_db
    }

    pub fn is_idle(&self) -> bool { self.offset_db == 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_scales_between_floor_and_range() {
        assert_eq!(target_db(-120.0, -12.0, 24.0), 0.0, "silent key: nothing");
        assert_eq!(target_db(FLOOR_DB, -12.0, 24.0), 0.0);
        assert!((target_db(-38.0, -12.0, 24.0) + 6.0).abs() < 1e-5, "half the range, half the gain");
        assert_eq!(target_db(0.0, -12.0, 24.0), -12.0, "full gain, never more");
        assert_eq!(target_db(0.0, 0.0, 24.0), 0.0, "Dyn Gain 0 is off");
    }

    #[test]
    fn attack_is_fast_release_is_slow_and_release_ends_at_exact_zero() {
        let sr = 48_000.0;
        let mut d = DynBand::default();
        let chunks = |ms: f32| (ms * 0.001 * sr / CHUNK as f32) as usize;
        for _ in 0..chunks(10.0) { d.step(-12.0, CHUNK, sr); }
        assert!(d.offset_db < -12.0 * 0.6, "one attack constant in: {}", d.offset_db);
        for _ in 0..chunks(200.0) { d.step(-12.0, CHUNK, sr); }
        for _ in 0..chunks(10.0) { d.step(0.0, CHUNK, sr); }
        assert!(d.offset_db < -10.0, "10 ms into a 120 ms release: {}", d.offset_db);
        for _ in 0..chunks(2000.0) { d.step(0.0, CHUNK, sr); }
        assert!(d.is_idle(), "{}", d.offset_db);
    }
}
