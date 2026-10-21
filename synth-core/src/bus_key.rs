//! Reacting to the bus key: another track's level, read from the SuperDuper
//! bus (`LiveScope::key_bands`, filled ~20×/s by the publisher thread).
//!
//! The key arrives 50–100 ms late (one publish period plus the analysis
//! window), so everything here is slow by design: it ducks a reverb tail
//! while a vocal phrase sings, it does not chase transients. A fast,
//! sample-accurate duck needs the sidechain port instead.
//!
//! Shared by Reverb and Delay (Key Duck), Filter (Key Mod) and Vocal (Rider).

use std::sync::atomic::Ordering;
use superduper_dsp_sdk::bus::BANDS;
use crate::gui::LiveScope;

/// The key's level that starts the reaction: the bus's "loud" line, as in
/// Soothe's Key Depth.
pub const KEY_FLOOR_DB: f32 = -50.0;
/// The frequency span that counts as "the key's level": where a vocal and
/// most of a mix live, without kick sub or cymbal air steering the result.
pub const KEY_LO_HZ: f32 = 100.0;
pub const KEY_HI_HZ: f32 = 8000.0;

/// Band `b`'s centre on the bus's 32-band log axis (20 Hz · 1000^((b+½)/32)).
fn band_centre(b: usize) -> f32 {
    20.0 * 1000f32.powf((b as f32 + 0.5) / BANDS as f32)
}

/// Power sum (dB) of the bands whose centre lies in `lo..=hi`. The bus bands
/// are band RMS levels that add up to the total, so this is the level of the
/// signal inside the span. −120 when nothing is there.
pub fn span_level_db(bands: &[f32; BANDS], lo: f32, hi: f32) -> f32 {
    let p: f32 = (0..BANDS)
        .filter(|&b| (lo..=hi).contains(&band_centre(b)))
        // −120 is the bus's "nothing": count it as no power at all.
        .map(|b| if !(bands[b] > -119.0) { 0.0 } else { 10f32.powf(bands[b].min(40.0) / 10.0) })
        .sum();
    (10.0 * p.max(1e-12).log10()).max(-120.0)
}

/// The key track's level right now (dB, 100 Hz–8 kHz). Atomics only, no
/// allocation: safe on the audio thread. −120 with no key picked.
pub fn key_level_db(scope: &LiveScope) -> f32 {
    let bands: [f32; BANDS] = std::array::from_fn(|b| scope.key_bands[b].load(Ordering::Relaxed));
    span_level_db(&bands, KEY_LO_HZ, KEY_HI_HZ)
}

/// 0 below the floor, 1 once the key is `range` dB above it, linear between.
pub fn key_factor(key_db: f32, range: f32) -> f32 {
    // A NaN key (a NaN on the bus) must read as "no key", never reach a gain.
    if !key_db.is_finite() || !range.is_finite() { return 0.0; }
    ((key_db - KEY_FLOOR_DB) / range.max(1.0)).clamp(0.0, 1.0)
}

/// Linear gain for a duck of `duck_db` at key factor `f` (0..1). Exactly 1.0
/// at factor 0 or depth 0, so an unkeyed plugin is bit-identical to before.
#[inline]
pub fn duck_gain(f: f32, duck_db: f32) -> f32 {
    if !(f > 0.0) || !(duck_db > 0.0) { 1.0 } else { 10f32.powf(-duck_db.min(120.0) * f.min(1.0) / 20.0) }
}

/// One-pole coefficient for a time constant in ms.
pub fn coef(ms: f32, sr: f32) -> f32 {
    (-1.0 / (ms.max(0.01) * 0.001 * sr.max(1.0))).exp()
}

/// One-pole follower with separate rise and fall times: the key factor rises
/// with the attack (the effect gets out of the way as the key starts) and
/// falls with the release (the tail blooms back between phrases).
#[derive(Clone, Copy, Debug, Default)]
pub struct Follower {
    pub y: f32,
}

impl Follower {
    #[inline]
    pub fn step(&mut self, target: f32, rise: f32, fall: f32) -> f32 {
        let c = if target > self.y { rise } else { fall };
        self.y = target + (self.y - target) * c;
        self.y
    }
}

/// The key factor, smoothed per sample: set the target once per block, step
/// it per sample.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyEnvelope {
    f: Follower,
    target: f32,
    rise: f32,
    fall: f32,
}

/// Attack and release of the key reaction (ms). The key itself is 50–100 ms
/// late, so a shorter attack buys nothing.
pub const KEY_ATTACK_MS: f32 = 20.0;
pub const KEY_RELEASE_MS: f32 = 250.0;

impl KeyEnvelope {
    /// Once per block: the factor to move toward (0 = off).
    pub fn set_target(&mut self, target: f32, sr: f32) {
        self.target = target;
        self.rise = coef(KEY_ATTACK_MS, sr);
        self.fall = coef(KEY_RELEASE_MS, sr);
    }
    /// Once per sample: the smoothed factor, 0..1.
    #[inline]
    pub fn tick(&mut self) -> f32 {
        self.f.step(self.target, self.rise, self.fall)
    }
    /// Read the key from the bus and set the target, once per block. With
    /// `amount` 0 the target is 0, so an unkeyed plugin glides to (and stays
    /// at) exactly no reaction.
    pub fn update(&mut self, scope: &LiveScope, amount: f32, range: f32, sr: f32) {
        let t = if amount != 0.0 { key_factor(key_level_db(scope), range) } else { 0.0 };
        self.set_target(t, sr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factor_scales_between_floor_and_range() {
        assert_eq!(key_factor(-120.0, 24.0), 0.0, "a silent key does nothing");
        assert_eq!(key_factor(KEY_FLOOR_DB, 24.0), 0.0);
        assert!((key_factor(-38.0, 24.0) - 0.5).abs() < 1e-6, "halfway up the range");
        assert_eq!(key_factor(-10.0, 24.0), 1.0, "full, never more");
    }

    #[test]
    fn span_level_sums_power_inside_the_span_only() {
        let mut b = [-120.0f32; BANDS];
        assert_eq!(span_level_db(&b, KEY_LO_HZ, KEY_HI_HZ), -120.0);
        // Two equal bands inside the span: +3 dB over one.
        b[16] = -20.0;
        b[17] = -20.0;
        let l = span_level_db(&b, KEY_LO_HZ, KEY_HI_HZ);
        assert!((l + 16.99).abs() < 0.05, "{l}");
        // A band below 100 Hz does not count.
        b[0] = 0.0;
        assert!((span_level_db(&b, KEY_LO_HZ, KEY_HI_HZ) - l).abs() < 1e-3);
    }

    #[test]
    fn envelope_attacks_fast_and_releases_slow() {
        let sr = 48_000.0;
        let mut e = KeyEnvelope::default();
        e.set_target(1.0, sr);
        for _ in 0..(0.1 * sr) as usize { e.tick(); }
        assert!(e.tick() > 0.99, "100 ms = 5 attack time constants");
        e.set_target(0.0, sr);
        for _ in 0..(0.1 * sr) as usize { e.tick(); }
        let v = e.tick();
        assert!(v > 0.6 && v < 0.75, "release is slower: e^(-100/250) ≈ 0.67, got {v}");
    }
}
