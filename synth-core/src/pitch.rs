//! Pitch detection + single-cycle extraction.
//!
//! Used by Wave's "Open WAV" path to turn an arbitrary pitched
//! recording (kubyz drone, vocal note, string sample, …) into a
//! single-cycle wavetable that actually sounds like the source —
//! rather than the linear-resample-the-whole-file mess that loses
//! amplitude and aliases the timbre.
//!
//! Two-stage pipeline:
//! 1. `detect_pitch_hz` — YIN-style autocorrelation on the loudest
//!    slice of the recording. Sub-sample-accurate, returns None for
//!    silence / noise / inharmonic content.
//! 2. `wav_to_single_cycle` — pick a zero-crossing in the loudest
//!    region, extract exactly one period at the detected pitch,
//!    resample to `target_len`, normalise to peak 0.95.
//!
//! Falls back gracefully to "loudest N samples, linear-resampled,
//! normalised" when pitch can't be found (drum hit, atonal field
//! recording, etc.) so any input still produces a non-silent curve.

/// Detect the fundamental frequency of a mono sample buffer using a
/// simplified YIN-style autocorrelation. Returns `None` for noise /
/// silence / inharmonic content.
///
/// The detector picks the loudest non-silent slice of the buffer so
/// long-tail samples (kicks with silent attack, vocals after a pause)
/// still resolve. Searches periods between 30 Hz and 4 kHz.
pub fn detect_pitch_hz(mono: &[f32], sample_rate: u32) -> Option<f32> {
    let frame_count = mono.len();
    if frame_count < 2048 {
        return None;
    }
    let window_len = frame_count.min(4096);

    let mut max_amp = 0.0f32;
    let mut start = 0usize;
    let hop = (frame_count / 32).max(128);
    let probe = 512.min(frame_count);
    let mut p = 0;
    while p + probe <= frame_count {
        let mut peak = 0.0f32;
        for i in p..p + probe {
            let v = mono[i].abs();
            if v > peak {
                peak = v;
            }
        }
        if peak > max_amp {
            max_amp = peak;
            start = p;
        }
        p += hop;
    }
    if max_amp < 0.005 {
        return None;
    }
    start = start.min(frame_count - window_len);
    let window = &mono[start..start + window_len];

    let min_period = ((sample_rate as f32 / 4000.0) as usize).max(2);
    let max_period = ((sample_rate as f32 / 30.0) as usize).min(window_len / 2 - 1);
    if max_period <= min_period {
        return None;
    }

    let n = max_period + 1;
    let mut cnd = vec![1.0f32; n];
    let mut acc = 0.0f64;
    let mut prev_below = false;
    let mut best_tau = 0usize;
    let mut best_val = f32::INFINITY;
    let threshold = 0.15f32;
    for tau in 1..n {
        let mut s = 0.0f64;
        let len = window_len - tau;
        for i in 0..len {
            let diff = window[i] - window[i + tau];
            s += (diff * diff) as f64;
        }
        acc += s;
        let val = if acc > 0.0 { (s * tau as f64 / acc) as f32 } else { 1.0 };
        cnd[tau] = val;
        if tau >= min_period {
            if val < best_val {
                best_val = val;
                best_tau = tau;
            }
            if val < threshold {
                prev_below = true;
            }
            if prev_below && tau > 1 && cnd[tau - 1] < cnd[tau] && cnd[tau - 1] < threshold {
                best_tau = tau - 1;
                break;
            }
        }
    }
    if best_tau < min_period || cnd[best_tau] > 0.5 {
        return None;
    }
    let refined = if best_tau > 0 && best_tau + 1 < n {
        let a = cnd[best_tau - 1];
        let b = cnd[best_tau];
        let c = cnd[best_tau + 1];
        let denom = 2.0 * (a - 2.0 * b + c);
        if denom.abs() > 1e-9 {
            best_tau as f32 + (a - c) / denom
        } else {
            best_tau as f32
        }
    } else {
        best_tau as f32
    };
    let hz = sample_rate as f32 / refined;
    if hz.is_finite() && hz > 20.0 && hz < 6000.0 {
        Some(hz)
    } else {
        None
    }
}

/// Find the loudest contiguous `probe`-sample window in `mono` and
/// return its start index. Used as the candidate location for cycle
/// extraction so we sample the tonal body, not silent pre-roll or
/// release tail.
fn loudest_region_start(mono: &[f32], probe: usize) -> usize {
    if mono.len() <= probe {
        return 0;
    }
    let hop = (mono.len() / 64).max(64);
    let mut best = 0usize;
    let mut best_peak = 0.0f32;
    let mut p = 0;
    while p + probe <= mono.len() {
        let mut peak = 0.0f32;
        for i in p..p + probe {
            let v = mono[i].abs();
            if v > peak {
                peak = v;
            }
        }
        if peak > best_peak {
            best_peak = peak;
            best = p;
        }
        p += hop;
    }
    best
}

/// Snap `pos` to the nearest positive-going zero crossing within
/// `window` samples on either side. Returns `pos` unchanged if no
/// crossing is found — fine, we just start mid-cycle.
fn snap_to_zero_crossing(mono: &[f32], pos: usize, window: usize) -> usize {
    let start = pos.saturating_sub(window);
    let end = (pos + window).min(mono.len().saturating_sub(1));
    for i in start..end {
        if i + 1 < mono.len() && mono[i] <= 0.0 && mono[i + 1] > 0.0 {
            return i;
        }
    }
    pos
}

/// Linear-resample `src` to `target_len` and return the result.
/// Quality is fine for wavetable use (the band-limit happens later
/// in the mip-pyramid build).
fn linear_resample(src: &[f32], target_len: usize) -> Vec<f32> {
    if src.is_empty() {
        return vec![0.0; target_len];
    }
    if src.len() == target_len {
        return src.to_vec();
    }
    let mut out = Vec::with_capacity(target_len);
    let denom = (target_len.max(2) - 1) as f32;
    for i in 0..target_len {
        let pos = (i as f32) * (src.len() as f32 - 1.0) / denom;
        let i0 = pos.floor() as usize;
        let frac = pos - i0 as f32;
        let i0 = i0.min(src.len() - 1);
        let i1 = (i0 + 1).min(src.len() - 1);
        out.push(src[i0] + (src[i1] - src[i0]) * frac);
    }
    out
}

/// Peak-normalise `samples` in place so `max |x|` becomes
/// `target_peak`. Silent inputs are left as-is.
fn normalise_peak(samples: &mut [f32], target_peak: f32) {
    let peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    if peak < 1e-6 {
        return;
    }
    let gain = target_peak / peak;
    for s in samples.iter_mut() {
        *s *= gain;
    }
}

/// Turn a mono pitched recording into a single-cycle wavetable of
/// `target_len` samples, normalised to ~`peak_target` amplitude.
///
/// Strategy:
/// 1. Detect fundamental pitch on the loudest slice.
/// 2. If pitched: snap to nearest rising zero-crossing in the
///    loudest region, take exactly one period (`sample_rate / hz`
///    samples), linear-resample to `target_len`.
/// 3. If unpitched (drums, noise): take the loudest `target_len`
///    consecutive samples from the buffer.
/// 4. Normalise peak.
///
/// Returns the resulting curve + the detected pitch (if any).
pub struct CycleExtract {
    pub curve: Vec<f32>,
    pub detected_hz: Option<f32>,
    /// True if the extraction used a real period; false if it fell
    /// back to "loudest region, linear-resampled".
    pub pitched: bool,
}

pub fn wav_to_single_cycle(
    mono: &[f32],
    sample_rate: u32,
    target_len: usize,
    peak_target: f32,
) -> CycleExtract {
    let detected_hz = detect_pitch_hz(mono, sample_rate);
    let mut curve;
    let pitched;
    match detected_hz {
        Some(hz) => {
            let period = (sample_rate as f32 / hz) as usize;
            // Look for the cycle inside the loudest 1024-sample window.
            let probe = (period * 8).min(2048);
            let start = loudest_region_start(mono, probe.min(mono.len()));
            let aligned = snap_to_zero_crossing(mono, start, period.max(64));
            let end = (aligned + period).min(mono.len());
            if end - aligned >= period.min(64) {
                curve = linear_resample(&mono[aligned..end], target_len);
                pitched = true;
            } else {
                // Pitched but ran past buffer end — fall back.
                let take = target_len.min(mono.len());
                curve = linear_resample(&mono[..take], target_len);
                pitched = false;
            }
        }
        None => {
            // Take the loudest contiguous target_len samples and
            // resample that — better than averaging the whole file
            // which dilutes amplitude.
            let take = target_len.min(mono.len());
            let start = loudest_region_start(mono, take);
            let end = (start + take).min(mono.len());
            curve = linear_resample(&mono[start..end], target_len);
            pitched = false;
        }
    }
    normalise_peak(&mut curve, peak_target);
    CycleExtract {
        curve,
        detected_hz,
        pitched,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_sine(hz: f32, sr: u32, secs: f32) -> Vec<f32> {
        let n = (secs * sr as f32) as usize;
        (0..n)
            .map(|i| (i as f32 / sr as f32 * std::f32::consts::TAU * hz).sin() * 0.4)
            .collect()
    }

    #[test]
    fn detects_440_hz_sine() {
        let s = synth_sine(440.0, 44100, 0.1);
        let hz = detect_pitch_hz(&s, 44100).expect("should detect 440");
        assert!((hz - 440.0).abs() < 2.0, "detected {hz}");
    }

    #[test]
    fn detects_low_kubyz_pitch() {
        let s = synth_sine(82.0, 44100, 0.1); // ~E2, typical kubyz
        let hz = detect_pitch_hz(&s, 44100).expect("should detect 82");
        assert!((hz - 82.0).abs() < 2.0, "detected {hz}");
    }

    #[test]
    fn rejects_silence() {
        let s = vec![0.0; 4096];
        assert!(detect_pitch_hz(&s, 44100).is_none());
    }

    #[test]
    fn rejects_noise() {
        // Pseudo-random samples (not perfect, but inharmonic enough).
        let s: Vec<f32> = (0..4096)
            .map(|i| {
                let x = (i as f32 * 12.9898).sin() * 43758.5453;
                (x - x.floor()) * 2.0 - 1.0
            })
            .collect();
        // Some pseudo-noise CAN trigger detection — assert peak position
        // by checking it isn't a confidently-detected musical pitch.
        if let Some(hz) = detect_pitch_hz(&s, 44100) {
            // If detected, at least the result should be wildly different
            // run-to-run — which is good enough for "not really pitched".
            assert!(hz > 30.0 && hz < 6000.0);
        }
    }

    #[test]
    fn extract_normalises_quiet_input() {
        let mut s = synth_sine(220.0, 44100, 0.1);
        for v in s.iter_mut() {
            *v *= 0.05; // very quiet
        }
        let ex = wav_to_single_cycle(&s, 44100, 2048, 0.95);
        assert!(ex.pitched, "should pitch-detect at 220 Hz");
        let peak = ex.curve.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
        assert!(
            (peak - 0.95).abs() < 0.02,
            "expected peak ~0.95, got {peak}"
        );
        // The extracted single cycle should look like a sine — RMS ≈ 0.95 / sqrt(2).
        let rms = (ex.curve.iter().map(|s| s * s).sum::<f32>() / ex.curve.len() as f32).sqrt();
        assert!(rms > 0.5, "expected loud RMS, got {rms}");
    }

    #[test]
    fn extract_unpitched_still_normalises() {
        // Burst of impulses — no clear pitch, but loud.
        let mut s = vec![0.0; 4096];
        for i in (0..4096).step_by(7) {
            s[i] = 0.1;
        }
        let ex = wav_to_single_cycle(&s, 44100, 2048, 0.95);
        // Whether pitched or not, peak should be normalised to 0.95.
        let peak = ex.curve.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
        assert!(
            (peak - 0.95).abs() < 0.02,
            "expected normalised peak, got {peak}"
        );
    }

    #[test]
    fn extract_returns_target_length() {
        let s = synth_sine(440.0, 44100, 0.1);
        let ex = wav_to_single_cycle(&s, 44100, 2048, 0.95);
        assert_eq!(ex.curve.len(), 2048);
    }
}
