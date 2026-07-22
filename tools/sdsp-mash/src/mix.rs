//! Pre-master mix — place stems on the grid, sum per role, sidechain-duck
//! `beat-other` from the vocal, sweep the beat bus. Pure in-memory: takes
//! decoded stems, returns the stereo pre-master bus. No file I/O, no plugins,
//! so it's directly unit-testable with synthetic buffers.

use crate::config::Role;
use crate::duck::{DuckParams, Ducker};
use crate::sweep::{apply_sweep, SweepParams};

/// A decoded stem placed on the grid: gain already resolved to linear,
/// offset already resolved to samples. `fade_in`/`fade_out` give equal-power
/// crossfade ramps (samples) used by section transitions.
pub struct Stem {
    pub role: Role,
    pub offset_samples: usize,
    pub gain: f32,
    pub fade_in: usize,
    pub fade_out: usize,
    pub l: Vec<f32>,
    pub r: Vec<f32>,
}

impl Stem {
    fn frames(&self) -> usize {
        self.l.len().min(self.r.len())
    }
    fn end(&self) -> usize {
        self.offset_samples + self.frames()
    }
    /// Equal-power fade gain at local sample `i` (of `frames` total).
    #[inline]
    fn fade_gain(&self, i: usize, frames: usize) -> f32 {
        let mut g = 1.0f32;
        if self.fade_in > 0 && i < self.fade_in {
            g *= ((i as f32 + 0.5) / self.fade_in as f32).clamp(0.0, 1.0).sqrt();
        }
        if self.fade_out > 0 {
            let from_end = frames.saturating_sub(i + 1);
            if from_end < self.fade_out {
                g *= ((from_end as f32 + 0.5) / self.fade_out as f32).clamp(0.0, 1.0).sqrt();
            }
        }
        g
    }
}

pub struct MixSettings {
    pub sr: u32,
    pub duck: Option<DuckParams>,
    pub sweep: Option<SweepParams>,
}

/// A pair of accumulation buffers.
struct Bus {
    l: Vec<f32>,
    r: Vec<f32>,
}

impl Bus {
    fn zeros(n: usize) -> Self {
        Bus {
            l: vec![0.0; n],
            r: vec![0.0; n],
        }
    }
    fn add_stem(&mut self, s: &Stem) {
        let frames = s.frames();
        let n = self.l.len();
        for i in 0..frames {
            let dst = s.offset_samples + i;
            if dst >= n {
                break;
            }
            let g = s.gain * s.fade_gain(i, frames);
            self.l[dst] += s.l[i] * g;
            self.r[dst] += s.r[i] * g;
        }
    }
}

/// Render the pre-master stereo bus from placed stems.
pub fn mix(stems: &[Stem], s: &MixSettings) -> (Vec<f32>, Vec<f32>) {
    let total = stems.iter().map(Stem::end).max().unwrap_or(0);
    if total == 0 {
        return (Vec::new(), Vec::new());
    }

    let mut drums = Bus::zeros(total);
    let mut bass = Bus::zeros(total);
    let mut other = Bus::zeros(total);
    let mut vocal = Bus::zeros(total);

    for st in stems {
        match st.role {
            Role::BeatDrums => drums.add_stem(st),
            Role::BeatBass => bass.add_stem(st),
            Role::BeatOther => other.add_stem(st),
            Role::Vocal => vocal.add_stem(st),
        }
    }

    // Sidechain ducking: the vocal keys a compressor on the `other` bus.
    // Bass is intentionally left untouched (holds the low end under the
    // vocal); drums punch through on their own.
    if let Some(p) = s.duck {
        let mut ducker = Ducker::new();
        let sr = s.sr as f32;
        for n in 0..total {
            let key = vocal.l[n].abs().max(vocal.r[n].abs());
            let g = ducker.gain_for(key, sr, &p);
            other.l[n] *= g;
            other.r[n] *= g;
        }
    }

    // Beat bus = drums + bass + ducked other.
    let mut beat_l = drums.l;
    let mut beat_r = drums.r;
    for n in 0..total {
        beat_l[n] += bass.l[n] + other.l[n];
        beat_r[n] += bass.r[n] + other.r[n];
    }

    // Intro sweep opens the beat bus.
    if let Some(sw) = s.sweep {
        apply_sweep(&mut beat_l, &mut beat_r, s.sr as f32, &sw);
    }

    // Master input = beat bus + vocal.
    for n in 0..total {
        beat_l[n] += vocal.l[n];
        beat_r[n] += vocal.r[n];
    }

    (beat_l, beat_r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn impulse_stem(role: Role, offset: usize, at: usize, len: usize) -> Stem {
        let mut l = vec![0.0; len];
        let mut r = vec![0.0; len];
        l[at] = 1.0;
        r[at] = 1.0;
        Stem {
            role,
            offset_samples: offset,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            l,
            r,
        }
    }

    #[test]
    fn stem_lands_at_offset_sample_accurate() {
        // Impulse at local sample 10, placed at offset 44_100.
        let stem = impulse_stem(Role::BeatDrums, 44_100, 10, 100);
        let settings = MixSettings {
            sr: 44_100,
            duck: None,
            sweep: None,
        };
        let (l, _r) = mix(&[stem], &settings);
        assert_eq!(l.len(), 44_100 + 100);
        // The impulse must be exactly at 44_100 + 10.
        assert!((l[44_110] - 1.0).abs() < 1e-6, "impulse misplaced");
        // Neighbours are silent.
        assert!(l[44_109].abs() < 1e-9);
        assert!(l[44_111].abs() < 1e-9);
    }

    #[test]
    fn gain_is_applied_linearly() {
        let mut stem = impulse_stem(Role::BeatBass, 0, 0, 10);
        stem.gain = 0.5;
        let settings = MixSettings {
            sr: 44_100,
            duck: None,
            sweep: None,
        };
        let (l, _r) = mix(&[stem], &settings);
        assert!((l[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn total_length_spans_latest_stem() {
        let a = impulse_stem(Role::BeatDrums, 0, 0, 100);
        let b = impulse_stem(Role::Vocal, 50_000, 0, 100);
        let settings = MixSettings {
            sr: 44_100,
            duck: None,
            sweep: None,
        };
        let (l, _r) = mix(&[a, b], &settings);
        assert_eq!(l.len(), 50_100);
    }

    #[test]
    fn ducking_reduces_other_under_vocal() {
        let sr = 44_100u32;
        let n = sr as usize; // 1 s
        // `other` = steady sine, `vocal` = loud burst in the second half.
        let f = 220.0f32;
        let other_l: Vec<f32> = (0..n)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * f * i as f32 / sr as f32).sin())
            .collect();
        let other = Stem {
            role: Role::BeatOther,
            offset_samples: 0,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            l:other_l.clone(),
            r: other_l.clone(),
        };
        // Vocal burst covers the second half [n/2, n).
        let mut voc_l = vec![0.0f32; n];
        for i in (n / 2)..n {
            voc_l[i] = 0.9 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / sr as f32).sin();
        }
        let vocal = Stem {
            role: Role::Vocal,
            offset_samples: 0,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            l:voc_l.clone(),
            r: voc_l.clone(),
        };
        let settings = MixSettings {
            sr,
            duck: Some(DuckParams {
                threshold_db: -30.0,
                ratio: 6.0,
                attack_ms: 5.0,
                release_ms: 80.0,
                knee_db: 6.0,
            }),
            sweep: None,
        };
        let (l, _r) = mix(&[other, vocal], &settings);

        // Compare `other`'s energy where there's no vocal vs. where the
        // vocal is loud. The output also contains the vocal itself, so
        // measure a window where vocal is present but subtract its known
        // contribution is messy — instead compare the FIRST quarter (no
        // vocal, full other) to a window right after the burst onset where
        // ducking has engaged, isolating `other` by using a low vocal freq
        // gap is hard. Simpler: rebuild with ducking off and compare the
        // ducked `other` bus indirectly via total RMS drop before/after.
        let pre = rms(&l[sr as usize / 8..sr as usize / 4]); // vocal-free
        // Window just after burst start, minus vocal — approximate by
        // comparing to the same render without ducking.
        let settings_noduck = MixSettings {
            sr,
            duck: None,
            sweep: None,
        };
        let other2 = Stem {
            role: Role::BeatOther,
            offset_samples: 0,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            l:other_l.clone(),
            r: other_l,
        };
        let vocal2 = Stem {
            role: Role::Vocal,
            offset_samples: 0,
            gain: 1.0,
            fade_in: 0,
            fade_out: 0,
            l:voc_l.clone(),
            r: voc_l,
        };
        let (l_noduck, _) = mix(&[other2, vocal2], &settings_noduck);

        // In the ducked half, the beat `other` is attenuated, so the ducked
        // render must have LESS energy than the un-ducked one there.
        let ducked_win = rms(&l[(3 * n / 4)..(3 * n / 4 + 4000)]);
        let open_win = rms(&l_noduck[(3 * n / 4)..(3 * n / 4 + 4000)]);
        assert!(
            ducked_win < open_win * 0.95,
            "ducking should lower energy under vocal: ducked {ducked_win}, open {open_win}"
        );
        // Sanity: the vocal-free head is identical between the two renders.
        let pre_noduck = rms(&l_noduck[sr as usize / 8..sr as usize / 4]);
        assert!((pre - pre_noduck).abs() < 1e-4, "head should be unaffected");
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }
}
