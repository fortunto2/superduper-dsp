//! The formant set as a value — 1 to 6 bands — plus the glue between it and
//! the plugin's atomics, the saved state and user presets.
//!
//! Where each band lives:
//! - bands 1–3: centre = the CLAP params F1/F2/F3 (so the vowel pad, the
//!   CC map and host automation keep driving them exactly as before);
//!   width / gain = `formant_bw[0..3]` / `formant_gain[0..3]`.
//! - bands 4–6: centre = `formant_hi_f[0..3]`, width / gain = the same
//!   arrays' upper slots. Not host-automatable — they are timbre, not
//!   gesture, the same status `formant_bw` / `formant_gain` always had.
//! - `formant_count` says how many leading bands are live (default 3, the
//!   value every pre-editor project and every factory preset implies).
//!
//! Each slot keeps its own frequency range — 1–3 inherit their param range,
//! 4–6 span the whole axis. `insert_sorted` places a new band by frequency so
//! the low slots keep the low formants and the ranges stay sensible.

use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;

use crate::{write_param, SharedParamsInner, PARAMS, P_F1};
pub use superduper_synth_core::kubyz::formant_bank::{FormantBands, MAX_FORMANTS};

/// Bands 1–3 are the CLAP params F1/F2/F3.
pub const PARAM_BANDS: usize = 3;
/// Never fewer than one resonance — zero would just be "Vox Mix = 0".
pub const MIN_FORMANTS: usize = 1;
/// What a pre-editor project / every factory preset means.
pub const DEFAULT_COUNT: usize = 3;
/// Range of the free bands (4–6): F1's floor to F3's ceiling.
pub const FREE_F_MIN: f32 = 80.0;
pub const FREE_F_MAX: f32 = 6000.0;
/// Linear gain limits (≈ −30 dB … +6 dB).
pub const GAIN_MIN: f32 = 0.0316;
pub const GAIN_MAX: f32 = 2.0;

/// One resonance: centre (Hz), bandwidth (Hz), linear gain.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Band {
    pub f: f32,
    pub bw: f32,
    pub gain: f32,
}

impl Band {
    pub fn is_valid(&self) -> bool {
        self.f.is_finite() && self.bw.is_finite() && self.bw > 0.0 && self.gain.is_finite()
    }
}

/// Defaults for the free slots 4–6 — upper vocal-tract resonances, quieter
/// than F1–F3. Only heard once the user adds them.
pub const FREE_DEFAULTS: [Band; MAX_FORMANTS - PARAM_BANDS] = [
    Band { f: 3200.0, bw: 450.0, gain: 0.5 },
    Band { f: 4200.0, bw: 550.0, gain: 0.4 },
    Band { f: 5200.0, bw: 650.0, gain: 0.3 },
];

/// Allowed centre range of slot `i`.
pub fn slot_range(i: usize) -> (f32, f32) {
    if i < PARAM_BANDS {
        let d = &PARAMS[P_F1 + i];
        (d.min as f32, d.max as f32)
    } else {
        (FREE_F_MIN, FREE_F_MAX)
    }
}

/// The whole set as a plain value — what the editor edits and the tests
/// exercise without a plugin instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FormantSet {
    pub bands: [Band; MAX_FORMANTS],
    pub count: usize,
}

impl FormantSet {
    /// Clamp every slot into its range and the count into 1..=6.
    pub fn clamp(&mut self) {
        self.count = self.count.clamp(MIN_FORMANTS, MAX_FORMANTS);
        for (i, b) in self.bands.iter_mut().enumerate() {
            let (lo, hi) = slot_range(i);
            b.f = b.f.clamp(lo, hi);
            b.bw = b.bw.clamp(20.0, 2.0 * b.f);
            b.gain = b.gain.clamp(GAIN_MIN, GAIN_MAX);
        }
    }

    /// Add a band, placed by frequency among the live ones (higher bands
    /// shift up a slot). Returns its slot, or `None` at the maximum.
    pub fn insert_sorted(&mut self, band: Band) -> Option<usize> {
        if self.count >= MAX_FORMANTS {
            return None;
        }
        let at = self.bands[..self.count].iter().take_while(|b| b.f < band.f).count();
        for i in (at..self.count).rev() {
            self.bands[i + 1] = self.bands[i];
        }
        self.bands[at] = band;
        self.count += 1;
        self.clamp();
        Some(at)
    }

    /// Remove the live band at `slot`; higher bands shift down. Refuses the
    /// last remaining band and slots that are not live.
    pub fn remove(&mut self, slot: usize) -> bool {
        if self.count <= MIN_FORMANTS || slot >= self.count {
            return false;
        }
        for i in slot..self.count - 1 {
            self.bands[i] = self.bands[i + 1];
        }
        // The vacated slot keeps a sane default rather than a duplicate.
        let last = self.count - 1;
        if last >= PARAM_BANDS {
            self.bands[last] = FREE_DEFAULTS[last - PARAM_BANDS];
        }
        self.count -= 1;
        self.clamp();
        true
    }

    /// The bank's view of the set (live bands only matter).
    pub fn to_bands(&self) -> FormantBands {
        FormantBands {
            f: std::array::from_fn(|i| self.bands[i].f),
            bw: std::array::from_fn(|i| self.bands[i].bw),
            gain: std::array::from_fn(|i| self.bands[i].gain),
            count: self.count,
        }
    }
}

/// Live band count from the atomic, clamped.
pub fn count(shared: &SharedParamsInner) -> usize {
    (shared.formant_count.load(Ordering::Relaxed) as usize).clamp(MIN_FORMANTS, MAX_FORMANTS)
}

/// Centre of slot `i` — params for 1–3, `formant_hi_f` for 4–6.
pub fn centre(shared: &SharedParamsInner, i: usize) -> f32 {
    if i < PARAM_BANDS {
        shared.params[P_F1 + i].load(Ordering::Relaxed)
    } else {
        shared.formant_hi_f[i - PARAM_BANDS].load(Ordering::Relaxed)
    }
}

/// Snapshot every slot (live or not) from the atomics.
pub fn load(shared: &SharedParamsInner) -> FormantSet {
    FormantSet {
        bands: std::array::from_fn(|i| Band {
            f: centre(shared, i),
            bw: shared.formant_bw[i].load(Ordering::Relaxed),
            gain: shared.formant_gain[i].load(Ordering::Relaxed),
        }),
        count: count(shared),
    }
}

/// Write a set back. Only values that changed are stored, so dragging band 2
/// does not stamp automation points onto F1 and F3. F1–F3 go through
/// `write_param` (dirty flag → host automation), the rest are plain stores.
pub fn store(shared: &SharedParamsInner, set: &FormantSet) {
    for (i, b) in set.bands.iter().enumerate() {
        if centre(shared, i) != b.f {
            if i < PARAM_BANDS {
                write_param(shared, P_F1 + i, b.f);
            } else {
                shared.formant_hi_f[i - PARAM_BANDS].store(b.f, Ordering::Relaxed);
            }
        }
        shared.formant_bw[i].store(b.bw, Ordering::Relaxed);
        shared.formant_gain[i].store(b.gain, Ordering::Relaxed);
    }
    shared.formant_count.store(set.count as u32, Ordering::Relaxed);
}

/// Bands 4–6 as saved in state / presets.
pub fn snapshot_free(shared: &SharedParamsInner) -> Vec<Band> {
    (PARAM_BANDS..MAX_FORMANTS)
        .map(|i| Band {
            f: centre(shared, i),
            bw: shared.formant_bw[i].load(Ordering::Relaxed),
            gain: shared.formant_gain[i].load(Ordering::Relaxed),
        })
        .collect()
}

/// Restore bands 4–6 + the count from saved state. A missing / short list
/// (old 3-formant saves) leaves the remaining slots alone; the count falls
/// back to 3 through the serde default upstream.
pub fn restore_free(shared: &SharedParamsInner, count: u32, free: &[Band]) {
    for (j, b) in free.iter().take(MAX_FORMANTS - PARAM_BANDS).enumerate() {
        if !b.is_valid() {
            continue;
        }
        let i = PARAM_BANDS + j;
        let (lo, hi) = slot_range(i);
        shared.formant_hi_f[j].store(b.f.clamp(lo, hi), Ordering::Relaxed);
        shared.formant_bw[i].store(b.bw, Ordering::Relaxed);
        shared.formant_gain[i].store(b.gain, Ordering::Relaxed);
    }
    let n = (count as usize).clamp(MIN_FORMANTS, MAX_FORMANTS);
    shared.formant_count.store(n as u32, Ordering::Relaxed);
}

/// serde default for every `formant_count` field: old saves had 3.
pub fn default_count() -> u32 {
    DEFAULT_COUNT as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set3() -> FormantSet {
        let mut bands = [Band { f: 0.0, bw: 0.0, gain: 0.0 }; MAX_FORMANTS];
        bands[0] = Band { f: 705.0, bw: 200.0, gain: 1.0 };
        bands[1] = Band { f: 1301.0, bw: 300.0, gain: 0.9 };
        bands[2] = Band { f: 2165.0, bw: 400.0, gain: 0.75 };
        bands[3..].copy_from_slice(&FREE_DEFAULTS);
        FormantSet { bands, count: 3 }
    }

    #[test]
    fn insert_places_by_frequency_and_stops_at_max() {
        let mut s = set3();
        let at = s.insert_sorted(Band { f: 300.0, bw: 80.0, gain: 1.0 });
        assert_eq!(at, Some(0));
        assert_eq!(s.count, 4);
        let fs: Vec<f32> = s.bands[..4].iter().map(|b| b.f).collect();
        assert_eq!(fs, vec![300.0, 705.0, 1301.0, 2165.0]);
        assert_eq!(s.insert_sorted(Band { f: 3000.0, bw: 300.0, gain: 0.5 }), Some(4));
        assert_eq!(s.insert_sorted(Band { f: 4000.0, bw: 300.0, gain: 0.5 }), Some(5));
        assert_eq!(s.insert_sorted(Band { f: 5000.0, bw: 300.0, gain: 0.5 }), None, "6 is the cap");
        assert_eq!(s.count, MAX_FORMANTS);
    }

    #[test]
    fn insert_clamps_into_the_slot_range() {
        let mut s = set3();
        s.count = 1;
        // Slot 1 is F2 (200..3500): a 5 kHz click lands at the ceiling.
        assert_eq!(s.insert_sorted(Band { f: 5000.0, bw: 300.0, gain: 1.0 }), Some(1));
        assert_eq!(s.bands[1].f, 3500.0);
    }

    #[test]
    fn remove_shifts_down_and_keeps_one() {
        let mut s = set3();
        assert!(s.remove(0));
        assert_eq!((s.count, s.bands[0].f, s.bands[1].f), (2, 1301.0, 2165.0));
        assert!(!s.remove(5), "a dead slot is not removable");
        assert!(s.remove(1));
        assert!(!s.remove(0), "the last band stays");
        assert_eq!(s.count, MIN_FORMANTS);
    }

    #[test]
    fn store_then_load_round_trips_through_the_atomics() {
        let shared = crate::PluginShared::new();
        let mut s = set3();
        s.insert_sorted(Band { f: 2900.0, bw: 350.0, gain: 0.6 }).unwrap();
        store(&shared.inner, &s);
        assert_eq!(load(&shared.inner), s);
        assert_eq!(count(&shared.inner), 4);
    }
}
