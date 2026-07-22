//! Phase auto-alignment — snap a vocal onto the beat by cross-correlating
//! onset envelopes.
//!
//! Two stages: a cheap coarse search over 50 Hz onset envelopes localises the
//! lag to within one env frame (±20 ms), then a fine sample-rate search over
//! the rectified onset signal pins it to the sample. The reported shift is
//! how many samples to add to the nominal offset for the best match.

use crate::onset::{onset_envelope, onset_signal};

const COARSE_RATE_HZ: f64 = 50.0;
/// Cap the correlation window so alignment focuses on the vocal's opening and
/// stays fast on long stems.
const MAX_CORR_SECS: f64 = 6.0;

pub struct AlignResult {
    /// Samples to add to the nominal offset (can be negative).
    pub shift_samples: i64,
    pub shift_ms: f64,
    /// Normalised correlation at the chosen lag (0..1-ish).
    pub score: f32,
}

#[inline]
fn xcorr(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut ea = 0.0f32;
    let mut eb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        ea += x * x;
        eb += y * y;
    }
    let denom = (ea * eb).sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        dot / denom
    }
}

/// Correlate `vocal` (starting at index 0) against `beat` starting at absolute
/// `beat_start`, over `len` positions. Out-of-range beat samples read 0.
fn corr_at(vocal: &[f32], beat: &[f32], beat_start: i64, len: usize) -> f32 {
    // Build the beat slice (with zero-padding for out-of-range) once.
    let bn = beat.len() as i64;
    let mut bslice = vec![0.0f32; len];
    for (k, slot) in bslice.iter_mut().enumerate() {
        let idx = beat_start + k as i64;
        if idx >= 0 && idx < bn {
            *slot = beat[idx as usize];
        }
    }
    xcorr(&vocal[..len], &bslice)
}

/// Find the sample shift that best aligns the vocal onset onto the beat-drums
/// onset, searching ±`search_samples` around `nominal_offset`.
///
/// - `vocal_l/r` — the vocal stem (local, already trimmed/stretched).
/// - `beat_l/r` — the beat-drums bus placed on the absolute timeline.
pub fn auto_align(
    vocal_l: &[f32],
    vocal_r: &[f32],
    beat_l: &[f32],
    beat_r: &[f32],
    sr: u32,
    nominal_offset: i64,
    search_samples: i64,
) -> AlignResult {
    // ---- Coarse stage: 50 Hz onset envelopes -----------------------------
    let voe = onset_envelope(vocal_l, vocal_r, sr, COARSE_RATE_HZ);
    let boe = onset_envelope(beat_l, beat_r, sr, COARSE_RATE_HZ);
    let hop = voe.hop as i64;

    if voe.env.is_empty() || boe.env.is_empty() || hop == 0 {
        return AlignResult {
            shift_samples: 0,
            shift_ms: 0.0,
            score: 0.0,
        };
    }

    // Correlation length in env frames.
    let max_frames = ((sr as f64 * MAX_CORR_SECS) / voe.hop as f64) as usize;
    let corr_frames = voe.env.len().min(max_frames).max(1);
    let nominal_frame = nominal_offset / hop;
    let search_frames = (search_samples / hop).max(1);

    let mut best_df = 0i64;
    let mut best_score = f32::NEG_INFINITY;
    let mut df = -search_frames;
    while df <= search_frames {
        let base = nominal_frame + df;
        let score = corr_at(&voe.env, &boe.env, base, corr_frames);
        if score > best_score {
            best_score = score;
            best_df = df;
        }
        df += 1;
    }
    let coarse_lag = best_df * hop; // shift (samples) from nominal

    // ---- Fine stage: sample-rate onset signals ±one hop ------------------
    let vos = onset_signal(vocal_l, vocal_r);
    let bos = onset_signal(beat_l, beat_r);
    let max_samps = (sr as f64 * MAX_CORR_SECS) as usize;
    let corr_samps = vos.len().min(max_samps).max(1);

    let mut best_fine = 0i64;
    let mut best_fscore = f32::NEG_INFINITY;
    let mut fine = -hop;
    while fine <= hop {
        let base = nominal_offset + coarse_lag + fine;
        let score = corr_at(&vos, &bos, base, corr_samps);
        if score > best_fscore {
            best_fscore = score;
            best_fine = fine;
        }
        fine += 1;
    }

    let shift = coarse_lag + best_fine;
    AlignResult {
        shift_samples: shift,
        shift_ms: 1000.0 * shift as f64 / sr as f64,
        score: best_fscore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;

    /// An irregular click pattern so the cross-correlation peak is unique
    /// within the search window (a periodic pattern would be ambiguous).
    fn click_track(n: usize, positions: &[usize]) -> Vec<f32> {
        let mut x = vec![0.0f32; n];
        for &p in positions {
            for k in 0..48 {
                if p + k < n {
                    let env = 1.0 - k as f32 / 48.0;
                    x[p + k] =
                        env * (2.0 * std::f32::consts::PI * 1200.0 * k as f32 / SR as f32).sin();
                }
            }
        }
        x
    }

    #[test]
    fn finds_injected_shift_within_1ms() {
        let n = SR as usize * 4; // 4 s timeline
        let positions = [
            SR as usize / 3,
            SR as usize / 2 + 7000,
            SR as usize,
            SR as usize * 2 - 3000,
            SR as usize * 2 + 15000,
        ];
        let beat = click_track(n, &positions);

        // The vocal *leads* the beat by a known amount (it plays the beat's
        // future). Placed at nominal offset 0, auto_align should recover a
        // shift of ≈ +inject to delay it back into place.
        let inject: i64 = 1234; // ~28 ms
        let mut vocal = vec![0.0f32; n];
        for i in 0..n {
            let src = i + inject as usize;
            if src < n {
                vocal[i] = beat[src];
            }
        }

        let res = auto_align(&vocal, &vocal, &beat, &beat, SR, 0, SR as i64 / 2);
        let err_ms = (res.shift_samples - inject).abs() as f64 * 1000.0 / SR as f64;
        assert!(
            err_ms < 1.0,
            "found shift {} ({} ms), injected {} — err {err_ms} ms",
            res.shift_samples,
            res.shift_ms,
            inject
        );
    }

    #[test]
    fn recovers_a_misconfigured_offset() {
        // Vocal and beat identical clicks; the config claims the vocal starts
        // 15 ms late. auto_align should pull it back so nominal+shift ≈ 0.
        let n = SR as usize * 3;
        let positions = [SR as usize / 4, SR as usize / 2, SR as usize, SR as usize + 9000];
        let beat = click_track(n, &positions);
        let nominal: i64 = (0.015 * SR as f64) as i64; // 15 ms wrong
        let res = auto_align(&beat, &beat, &beat, &beat, SR, nominal, SR as i64 / 2);
        let residual = (nominal + res.shift_samples).abs() as f64 * 1000.0 / SR as f64;
        assert!(residual < 1.0, "residual {residual} ms after align");
    }
}
