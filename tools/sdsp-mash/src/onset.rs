//! Onset-strength envelopes — the raw material for phase auto-alignment and
//! BPM estimation.
//!
//! Two granularities:
//! - [`onset_envelope`] downsamples to a chosen rate (50 Hz for alignment,
//!   ~200 Hz for BPM): each frame is the RMS of the half-wave-rectified
//!   amplitude increase over that frame's hop.
//! - [`onset_signal`] keeps sample-rate resolution (one value per sample) so
//!   a coarse env-domain lag can be refined to sample accuracy.

/// A downsampled onset envelope plus the hop that produced it (so callers can
/// convert frame index ↔ sample index).
pub struct OnsetEnv {
    pub env: Vec<f32>,
    pub hop: usize,
}

/// Per-sample onset strength: half-wave-rectified first difference of the
/// mono amplitude, `max(0, |x[i]| - |x[i-1]|)`. Spiky at transients.
pub fn onset_signal(l: &[f32], r: &[f32]) -> Vec<f32> {
    let n = l.len().min(r.len());
    let mut out = vec![0.0f32; n];
    let mut prev = 0.0f32;
    for i in 0..n {
        let amp = 0.5 * (l[i].abs() + r[i].abs());
        let d = amp - prev;
        out[i] = if d > 0.0 { d } else { 0.0 };
        prev = amp;
    }
    out
}

/// Onset envelope downsampled to `rate_hz`: RMS of the per-sample onset
/// strength over each `hop = sr / rate_hz` block.
pub fn onset_envelope(l: &[f32], r: &[f32], sr: u32, rate_hz: f64) -> OnsetEnv {
    let hop = ((sr as f64 / rate_hz).round() as usize).max(1);
    let os = onset_signal(l, r);
    let frames = os.len() / hop;
    let mut env = Vec::with_capacity(frames);
    for f in 0..frames {
        let base = f * hop;
        let mut sq = 0.0f32;
        for k in 0..hop {
            let v = os[base + k];
            sq += v * v;
        }
        env.push((sq / hop as f32).sqrt());
    }
    OnsetEnv { env, hop }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onset_env_peaks_at_a_transient() {
        let sr = 44_100u32;
        let n = sr as usize; // 1 s
        let mut x = vec![0.0f32; n];
        // Silence, then a loud burst at 0.5 s.
        let start = n / 2;
        for k in 0..2000 {
            x[start + k] = 0.8 * (2.0 * std::f32::consts::PI * 300.0 * k as f32 / sr as f32).sin();
        }
        let oe = onset_envelope(&x, &x, sr, 50.0);
        // The frame covering the burst onset should be the strongest.
        let onset_frame = start / oe.hop;
        let peak_frame = oe
            .env
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert!(
            (peak_frame as i64 - onset_frame as i64).abs() <= 1,
            "onset peak at frame {peak_frame}, expected ~{onset_frame}"
        );
    }
}
