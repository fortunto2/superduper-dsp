//! Vocal rider (Mixing Secrets ch19): ride the vocal's gain so it sits a set
//! number of dB over the bus key, which here is the backing — the
//! instrumental bus, or the master.
//!
//! Own level: a mean-square follower (100 ms) on this plugin's input, before
//! any processing, so the rider never chases its own output. A fast (10 ms)
//! follower gates it: when the vocal stops or dips 10 dB under its own level,
//! the slow level freezes (leaking with a 2 s time constant, so a vocal that
//! comes back much quieter is followed eventually) and the gain holds —
//! otherwise every phrase end would read as "the vocal got quiet" and pull
//! the gain up into the gap. Key level: the
//! key track's 100 Hz–8 kHz level from the bus (`key::key_level_db`), 50–100
//! ms late. The gain moves slowly (300 ms) and only while both are present:
//! a silent key glides back to 0 dB, a silent vocal holds the last gain so
//! breaths and gaps are not pulled up.
//!
//! With the master as key, the master contains this vocal: raising it raises
//! the key too, so a target above the vocal's natural share of the mix runs
//! to Rider Max. Key the instrumental bus when there is one; with the master,
//! keep the target at or below 0 dB.
//!
//! How to key the master: the GUI picker lists ordinary tracks only, so an
//! agent sets it through the bus mailbox's "key" document, e.g.
//! `plugin_set(track, "Vocal", "key", {"track": "Master"})` (the master is
//! named "Master" on the bus; it needs a SuperDuper plugin on it to publish).

use crate::key;
use superduper_synth_core::gui::LiveScope;

/// Below this the key counts as silent: no riding, glide back to 0 dB.
pub const KEY_SILENT_DB: f32 = -70.0;
/// Below this the vocal counts as silent: hold the gain.
pub const VOCAL_SILENT_DB: f32 = -55.0;
const DETECT_MS: f32 = 100.0;
const GATE_MS: f32 = 10.0;
const HELD_LEAK_MS: f32 = 2000.0;
const RIDE_MS: f32 = 300.0;
/// Fast level this far under the slow one = a phrase ending or a gap: hold.
const DIP_DB: f32 = 10.0;

#[derive(Clone, Copy, Debug, Default)]
pub struct Rider {
    ms: f32,
    fast_ms: f32,
    gain_db: f32,
    // Per block.
    key_db: f32,
    amount: f32,
    target_db: f32,
    max_db: f32,
    c_detect: f32,
    c_gate: f32,
    c_leak: f32,
    c_ride: f32,
}

impl Rider {
    /// Once per block: read the key from the bus and take the knobs.
    pub fn update(&mut self, scope: &LiveScope, amount: f32, target_db: f32, max_db: f32, sr: f32) {
        let key_db = if amount > 0.0 { key::key_level_db(scope) } else { -120.0 };
        self.set_block(key_db, amount, target_db, max_db, sr);
    }

    pub fn set_block(&mut self, key_db: f32, amount: f32, target_db: f32, max_db: f32, sr: f32) {
        self.key_db = key_db;
        self.amount = amount.clamp(0.0, 1.0);
        self.target_db = target_db;
        self.max_db = max_db.max(0.0);
        self.c_detect = key::coef(DETECT_MS, sr);
        self.c_gate = key::coef(GATE_MS, sr);
        self.c_leak = key::coef(HELD_LEAK_MS, sr);
        self.c_ride = key::coef(RIDE_MS, sr);
    }

    /// The gain now (dB), for the meter.
    pub fn gain_db(&self) -> f32 { self.gain_db }

    /// Once per sample, with this sample's input: the linear gain to apply.
    /// Exactly 1.0 while off and settled, so an unridden vocal is unchanged.
    #[inline]
    pub fn step(&mut self, l: f32, r: f32) -> f32 {
        if self.amount <= 0.0 && self.gain_db == 0.0 {
            return 1.0;
        }
        let x2 = 0.5 * (l * l + r * r);
        self.fast_ms = x2 + (self.fast_ms - x2) * self.c_gate;
        let fast_db = 10.0 * self.fast_ms.max(1e-12).log10();
        let slow_db = 10.0 * self.ms.max(1e-12).log10();
        let held = fast_db <= VOCAL_SILENT_DB || fast_db < slow_db - DIP_DB;
        let c = if held { self.c_leak } else { self.c_detect };
        self.ms = x2 + (self.ms - x2) * c;
        let own_db = 10.0 * self.ms.max(1e-12).log10();
        let target = if self.amount <= 0.0 || self.key_db <= KEY_SILENT_DB {
            0.0
        } else if held {
            self.gain_db
        } else {
            self.amount * (self.key_db + self.target_db - own_db).clamp(-self.max_db, self.max_db)
        };
        self.gain_db = target + (self.gain_db - target) * self.c_ride;
        if target == 0.0 && self.gain_db.abs() < 1e-4 {
            self.gain_db = 0.0;
        }
        10f32.powf(self.gain_db / 20.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    /// A steady vocal at `vocal_db` RMS for two seconds against a key at
    /// `key_db`; returns the settled gain in dB.
    fn settle(vocal_db: f32, key_db: f32, amount: f32, target: f32, max: f32) -> f32 {
        let mut r = Rider::default();
        r.set_block(key_db, amount, target, max, SR);
        let a = 10f32.powf(vocal_db / 20.0) * std::f32::consts::SQRT_2;
        let mut g = 1.0;
        for i in 0..(2.0 * SR) as usize {
            let x = a * (i as f32 * 0.05).sin();
            g = r.step(x, x);
        }
        20.0 * g.log10()
    }

    #[test]
    fn off_is_exactly_unity() {
        let mut r = Rider::default();
        r.set_block(-10.0, 0.0, 3.0, 6.0, SR);
        assert!((0..10_000).all(|i| r.step((i as f32).sin(), 0.3) == 1.0));
    }

    #[test]
    fn rides_toward_target_over_the_key() {
        // Vocal −20, key −26, target +3: wants −26 + 3 = −23 → −3 dB.
        let g = settle(-20.0, -26.0, 1.0, 3.0, 6.0);
        assert!((g + 3.0).abs() < 0.2, "gain {g:.2} dB, want −3");
        // Half the amount, half the move.
        let g = settle(-20.0, -26.0, 0.5, 3.0, 6.0);
        assert!((g + 1.5).abs() < 0.2, "gain {g:.2} dB, want −1.5");
    }

    #[test]
    fn clamps_to_rider_max() {
        let g = settle(-30.0, -10.0, 1.0, 3.0, 6.0);
        assert!((g - 6.0).abs() < 0.1, "gain {g:.2} dB, want +6 (clamped from +23)");
        let g = settle(-6.0, -40.0, 1.0, 0.0, 4.0);
        assert!((g + 4.0).abs() < 0.1, "gain {g:.2} dB, want −4");
    }

    #[test]
    fn silent_key_does_nothing() {
        assert_eq!(settle(-20.0, -120.0, 1.0, 3.0, 6.0), 0.0);
    }

    #[test]
    fn silent_vocal_holds_the_gain() {
        let mut r = Rider::default();
        r.set_block(-26.0, 1.0, 3.0, 6.0, SR);
        let a = 0.1 * std::f32::consts::SQRT_2; // −20 dB RMS
        for i in 0..(2.0 * SR) as usize { let x = a * (i as f32 * 0.05).sin(); r.step(x, x); }
        let held = r.gain_db();
        for _ in 0..(2.0 * SR) as usize { r.step(0.0, 0.0); }
        assert!((r.gain_db() - held).abs() < 0.3, "held {held:.2}, now {:.2}", r.gain_db());
    }
}
