//! Mix map: which tracks fight over which frequencies, from the per-track
//! band spectra on the SuperDuper bus. One implementation shared by the
//! Spectrum plugin's Mix view, the bus gateway and the offline `mixmap.py`
//! picture (which mirrors these rules).
//!
//! A clash in one band at one moment = both tracks are loud there
//! (> `LOUD_DB`) and within `CLOSE_DB` of each other — neither clearly owns
//! the band. The tracker keeps, per pair and band, the share of recent time
//! that held (exponential window), so a clash that happens on every chorus
//! hit rises and one stray transient does not.
//!
//! Masters and other unnamed buses are left out: they carry every track, so
//! they would "clash" with all of them.

use std::collections::HashMap;

pub const BANDS: usize = 32;
#[cfg(feature = "gui")]
const _: () = assert!(BANDS == superduper_dsp_sdk::bus::BANDS);
pub const LOUD_DB: f32 = -50.0;
pub const CLOSE_DB: f32 = 6.0;

/// Master and unnamed buses: excluded from masking.
pub fn is_bus_name(name: &str) -> bool {
    let n = name.trim().to_lowercase();
    n.is_empty() || n == "master" || n == "мастер"
}

/// One track's current spectrum (the loudest instance on that track).
#[derive(Debug, Clone, PartialEq)]
pub struct TrackSpectrum {
    pub name: String,
    pub rms_db: f32,
    pub bands_db: [f32; BANDS],
}

/// Collapse bus rows `(track, rms, bands)` to one per track: the loudest
/// instance, which is usually the track's output stage (the bus has no
/// plugin order).
pub fn loudest_per_track<'a>(rows: impl IntoIterator<Item = (&'a str, f32, &'a [f32; BANDS])>) -> Vec<TrackSpectrum> {
    let mut by: HashMap<&str, TrackSpectrum> = HashMap::new();
    for (name, rms, bands) in rows {
        let e = by.entry(name).or_insert_with(|| TrackSpectrum { name: name.to_string(), rms_db: f32::NEG_INFINITY, bands_db: *bands });
        if rms > e.rms_db { e.rms_db = rms; e.bands_db = *bands; }
    }
    let mut v: Vec<TrackSpectrum> = by.into_values().collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

#[derive(Debug, Clone, PartialEq)]
pub struct Clash {
    pub a: String,
    pub b: String,
    /// The band where the clash is most persistent.
    pub band: usize,
    /// Share of recent time that band clashed, 0..1.
    pub share: f32,
    /// Sum of shares over all bands — how much of the spectrum they share.
    pub total: f32,
}

pub struct MaskingTracker {
    /// Per-update weight of the newest frame (1/window); `None` = plain mean
    /// over every update so far.
    alpha: Option<f32>,
    frames: u32,
    pairs: HashMap<(String, String), [f32; BANDS]>,
}

impl MaskingTracker {
    /// Live view: `window_frames` is how many updates the exponential window
    /// spans, so old clashes fade.
    pub fn new(window_frames: f32) -> Self {
        Self { alpha: Some(1.0 / window_frames.max(1.0)), frames: 0, pairs: HashMap::new() }
    }

    /// A fixed listening span: share = clashing updates / all updates. An
    /// exponential window as long as the span would report a clash that held
    /// the whole time as 63%.
    pub fn mean() -> Self {
        Self { alpha: None, frames: 0, pairs: HashMap::new() }
    }

    pub fn update(&mut self, tracks: &[TrackSpectrum]) {
        let real: Vec<&TrackSpectrum> = tracks.iter().filter(|t| !is_bus_name(&t.name)).collect();
        self.frames += 1;
        // Running mean is the same recurrence with weight 1/n.
        let a = self.alpha.unwrap_or(1.0 / self.frames as f32);
        // Decay pairs that are no longer both present.
        for v in self.pairs.values_mut() { for x in v.iter_mut() { *x *= 1.0 - a; } }
        for (i, x) in real.iter().enumerate() {
            for y in &real[i + 1..] {
                let key = if x.name <= y.name { (x.name.clone(), y.name.clone()) } else { (y.name.clone(), x.name.clone()) };
                let slot = self.pairs.entry(key).or_insert([0.0; BANDS]);
                for b in 0..BANDS {
                    let (p, q) = (x.bands_db[b], y.bands_db[b]);
                    let clash = p > LOUD_DB && q > LOUD_DB && (p - q).abs() < CLOSE_DB;
                    // The decay above already took (1 − a); add the new sample.
                    if clash { slot[b] += a; }
                }
            }
        }
        self.pairs.retain(|_, v| v.iter().any(|x| *x > 1e-3));
    }

    /// The `n` most persistent clashes, strongest first.
    pub fn top(&self, n: usize) -> Vec<Clash> {
        let mut v: Vec<Clash> = self.pairs.iter().map(|((a, b), s)| {
            let band = (0..BANDS).max_by(|&i, &j| s[i].total_cmp(&s[j])).unwrap_or(0);
            Clash { a: a.clone(), b: b.clone(), band, share: s[band], total: s.iter().sum() }
        }).collect();
        v.sort_by(|x, y| y.share.total_cmp(&x.share).then(y.total.total_cmp(&x.total)));
        v.truncate(n);
        v
    }

    /// Per-band share for one pair (for drawing), if tracked.
    pub fn pair(&self, a: &str, b: &str) -> Option<[f32; BANDS]> {
        let key = if a <= b { (a.to_string(), b.to_string()) } else { (b.to_string(), a.to_string()) };
        self.pairs.get(&key).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(name: &str, loud: &[(usize, f32)]) -> TrackSpectrum {
        let mut b = [-120.0; BANDS];
        for &(i, v) in loud { b[i] = v; }
        TrackSpectrum { name: name.into(), rms_db: -20.0, bands_db: b }
    }

    #[test]
    fn persistent_overlap_is_a_clash_level_gap_is_not() {
        let mut m = MaskingTracker::new(10.0);
        for _ in 0..60 {
            m.update(&[
                t("Bass", &[(8, -20.0), (12, -25.0)]),
                t("Guitar", &[(12, -27.0), (20, -30.0)]),
                t("Pad", &[(12, -45.0)]), // 20 dB under the bass: the bass owns it
                t("Master", &[(8, -10.0), (12, -12.0)]),
            ]);
        }
        let top = m.top(5);
        assert_eq!((top[0].a.as_str(), top[0].b.as_str(), top[0].band), ("Bass", "Guitar", 12));
        assert!(top[0].share > 0.95, "{}", top[0].share);
        assert!(top.iter().all(|c| c.a != "Master" && c.b != "Master"), "master excluded");
        assert!(m.pair("Bass", "Pad").is_none_or(|s| s[12] < 0.01), "a 20 dB gap is ownership, not masking");
    }

    #[test]
    fn a_clash_fades_when_it_stops() {
        let mut m = MaskingTracker::new(10.0);
        for _ in 0..30 { m.update(&[t("A", &[(5, -20.0)]), t("B", &[(5, -22.0)])]); }
        let before = m.top(1)[0].share;
        for _ in 0..30 { m.update(&[t("A", &[(5, -20.0)]), t("B", &[(5, -60.0)])]); }
        let after = m.top(1).first().map_or(0.0, |c| c.share);
        assert!(before > 0.9 && after < 0.1, "{before} → {after}");
    }

    #[test]
    fn mean_reports_the_share_of_the_span() {
        let mut m = MaskingTracker::mean();
        for i in 0..40 {
            let b = if i < 30 { -22.0 } else { -60.0 }; // clashes 30 of 40 updates
            m.update(&[t("A", &[(5, -20.0)]), t("B", &[(5, b)])]);
        }
        let s = m.top(1)[0].share;
        assert!((s - 0.75).abs() < 1e-4, "{s}");
    }

    #[test]
    fn loudest_instance_stands_for_the_track() {
        let a = [-30.0; BANDS];
        let b = [-10.0; BANDS];
        let v = loudest_per_track([("Vox", -30.0, &a), ("Vox", -10.0, &b), ("Kick", -12.0, &a)]);
        assert_eq!(v.len(), 2);
        assert_eq!(v.iter().find(|t| t.name == "Vox").map(|t| t.bands_db[0]), Some(-10.0));
    }
}
