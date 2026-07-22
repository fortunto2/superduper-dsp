//! Intro lowpass sweep — the beat bus opens from a low cutoff to a high one
//! over the first N bars, so the drop lands when the filter fully opens.
//!
//! Cutoff rises *exponentially* (linear in log-frequency) across the sweep
//! so the motion sounds even to the ear. Past the sweep window the filter is
//! bypassed (fully open). Coefficients are recomputed once per small block
//! (not per sample) — the ear can't hear the stair-step and it's far cheaper.

use superduper_synth_core::dsp_blocks::Biquad;

/// How often to recompute the moving lowpass coefficients, in samples.
const COEFF_BLOCK: usize = 64;
/// Resonance (Q) of the sweeping lowpass. A touch above Butterworth for a
/// little emphasis at the cutoff as it climbs.
const SWEEP_Q: f32 = 0.9;

/// Resolved sweep parameters.
#[derive(Debug, Clone, Copy)]
pub struct SweepParams {
    /// Length of the sweep in samples (bars × beats-per-bar × spb × sr).
    pub len_samples: usize,
    pub from_hz: f32,
    pub to_hz: f32,
}

impl SweepParams {
    /// Cutoff (Hz) at absolute sample index `n`. Exponential from `from_hz`
    /// to `to_hz` across `[0, len_samples)`; `to_hz` at/after the end.
    #[inline]
    pub fn cutoff_at(&self, n: usize) -> f32 {
        if self.len_samples == 0 || n >= self.len_samples {
            return self.to_hz;
        }
        let t = n as f32 / self.len_samples as f32;
        // exp interpolation: from * (to/from)^t
        self.from_hz * (self.to_hz / self.from_hz).powf(t)
    }
}

/// Apply the sweep in-place to a stereo beat bus. Nyquist-safe: once the
/// cutoff reaches ~45% of SR the filter is bypassed (already effectively
/// open, and RBJ lowpass coefficients get unstable near Nyquist).
pub fn apply_sweep(l: &mut [f32], r: &mut [f32], sr: f32, p: &SweepParams) {
    let n = l.len().min(r.len());
    if n == 0 || p.len_samples == 0 {
        return;
    }
    let nyq_guard = sr * 0.45;
    let mut fl = Biquad::default();
    let mut fr = Biquad::default();
    let mut i = 0;
    while i < n {
        let cutoff = p.cutoff_at(i).clamp(20.0, nyq_guard);
        let open = cutoff >= nyq_guard - 1.0;
        if !open {
            fl.set_lpf(sr, cutoff, SWEEP_Q);
            fr.set_lpf(sr, cutoff, SWEEP_Q);
        }
        let end = (i + COEFF_BLOCK).min(n);
        if open {
            // Past the sweep or above the guard — leave the bus untouched,
            // but keep filter state warm so re-entry (shouldn't happen, the
            // sweep is monotonic) wouldn't click.
            for j in i..end {
                fl.process(l[j]);
                fr.process(r[j]);
            }
        } else {
            for j in i..end {
                l[j] = fl.process(l[j]);
                r[j] = fr.process(r[j]);
            }
        }
        i = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutoff_endpoints_and_monotonic() {
        let p = SweepParams {
            len_samples: 1000,
            from_hz: 200.0,
            to_hz: 20000.0,
        };
        assert!((p.cutoff_at(0) - 200.0).abs() < 1e-3);
        // At the end index the cutoff is at to_hz.
        assert!((p.cutoff_at(1000) - 20000.0).abs() < 1.0);
        // Midpoint (t=0.5) is the geometric mean, not the arithmetic one.
        let mid = p.cutoff_at(500);
        let geo = (200.0f32 * 20000.0).sqrt();
        assert!((mid - geo).abs() / geo < 0.02, "mid {mid} vs geo {geo}");
        // Strictly increasing.
        let mut prev = 0.0;
        for n in (0..1000).step_by(50) {
            let c = p.cutoff_at(n);
            assert!(c > prev, "cutoff not monotonic at {n}");
            prev = c;
        }
    }

    #[test]
    fn sweep_attenuates_early_high_freq_content() {
        // A bright signal (near-Nyquist tone) should be quieter in the first
        // block (cutoff low) than after the sweep opens.
        let sr = 44_100.0;
        let len = sr as usize; // 1 s of sweep
        let total = 2 * len;
        let f = 8000.0;
        let mut l: Vec<f32> = (0..total)
            .map(|n| (2.0 * std::f32::consts::PI * f * n as f32 / sr).sin())
            .collect();
        let mut r = l.clone();
        let p = SweepParams {
            len_samples: len,
            from_hz: 150.0,
            to_hz: 20000.0,
        };
        apply_sweep(&mut l, &mut r, sr, &p);

        // Early window (cutoff well below 8 kHz) should be heavily attenuated.
        let early_rms = rms(&l[2000..6000]);
        // Late window (past the sweep, filter open) keeps the tone.
        let late_rms = rms(&l[total - 6000..total - 2000]);
        assert!(
            early_rms < late_rms * 0.5,
            "sweep should attenuate early highs: early {early_rms}, late {late_rms}"
        );
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }
}
