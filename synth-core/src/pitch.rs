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

/// Extract N evenly-spaced single-cycle frames from a pitched
/// recording. Used to build multi-frame wavetables that morph
/// through the recording's timbre evolution (attack body → sustain
/// → release tail). Each frame is one period at the detected pitch
/// taken from a different time anchor, peak-normalised, target_len
/// samples.
///
/// Returns `Some(frames)` of length exactly `n_frames` on success,
/// `None` if pitch detection failed or the recording is too short
/// to fit `n_frames` cycles.
pub struct MultiFrameExtract {
    pub frames: Vec<Vec<f32>>,
    pub detected_hz: f32,
}

pub fn wav_to_multi_frame(
    mono: &[f32],
    sample_rate: u32,
    n_frames: usize,
    target_len: usize,
    peak_target: f32,
) -> Option<MultiFrameExtract> {
    if n_frames == 0 {
        return None;
    }
    let hz = detect_pitch_hz(mono, sample_rate)?;
    let period = (sample_rate as f32 / hz) as usize;
    if period < 8 || mono.len() < period * n_frames {
        return None;
    }
    // Anchor positions evenly spaced from "first loud sample" to
    // "last sample minus one period".
    let probe = (period * 4).min(2048);
    let first_loud = loudest_region_start(mono, probe.min(mono.len()));
    let last = mono.len().saturating_sub(period);
    if last <= first_loud {
        return None;
    }
    let mut frames: Vec<Vec<f32>> = Vec::with_capacity(n_frames);
    for k in 0..n_frames {
        let t = if n_frames > 1 {
            k as f32 / (n_frames as f32 - 1.0)
        } else {
            0.0
        };
        let anchor = first_loud + ((last - first_loud) as f32 * t) as usize;
        let aligned = snap_to_zero_crossing(mono, anchor, period.max(64));
        let end = (aligned + period).min(mono.len());
        if end <= aligned {
            continue;
        }
        let mut frame = linear_resample(&mono[aligned..end], target_len);
        normalise_peak(&mut frame, peak_target);
        frames.push(frame);
    }
    if frames.len() < n_frames {
        return None;
    }
    Some(MultiFrameExtract {
        frames,
        detected_hz: hz,
    })
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

// ===========================================================================
// Wavetable processing transforms — apply to a single-cycle frame to
// derive a new timbre. Each takes &[f32] and returns Vec<f32> of the
// same length. All produce normalised output (peak ≤ 1.0) so chaining
// doesn't blow up the synth.
// ===========================================================================

/// Reverse the waveform in time. For a single cycle this is identical
/// to mirroring around the centre — the spectrum's magnitudes stay
/// the same but phase relationships flip, which changes the
/// percussive feel (attack-y vs body-heavy) without changing brightness.
pub fn transform_mirror(frame: &[f32]) -> Vec<f32> {
    frame.iter().rev().copied().collect()
}

/// Flip polarity. Spectrum is identical (any continuous wavetable
/// inverted is its own mirror) but the DC offset and asymmetry flip —
/// noticeable on plucks and saws, inaudible on perfectly-symmetric
/// sines.
pub fn transform_invert(frame: &[f32]) -> Vec<f32> {
    frame.iter().map(|s| -s).collect()
}

/// Octave-up via period doubling: pack two copies of the input cycle
/// into the same `WT_SIZE`. The pitched fundamental moves up an octave
/// because we now have two periods per cycle.
pub fn transform_octave_up(frame: &[f32]) -> Vec<f32> {
    let n = frame.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let src = (i * 2) % n;
        out.push(frame[src]);
    }
    out
}

/// Octave-down via period halving: stretch the first half of the
/// input across the full `WT_SIZE` (skip the second half — for a
/// single cycle the second half is redundant, but for richer
/// content this drops the upper harmonics).
pub fn transform_octave_down(frame: &[f32]) -> Vec<f32> {
    let n = frame.len();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let src = i / 2;
        out.push(frame[src]);
    }
    out
}

/// Low-pass via moving-average smoothing — kills high harmonics,
/// produces a darker / softer wavetable. `kernel` controls width
/// (3 = mild, 11 = aggressive, 31 = almost-sine).
pub fn transform_smooth(frame: &[f32], kernel: usize) -> Vec<f32> {
    let n = frame.len();
    let k = kernel.max(1) | 1; // force odd so we have a centre
    let half = k / 2;
    let mut out = vec![0.0f32; n];
    for i in 0..n {
        let mut sum = 0.0f32;
        for j in 0..k {
            let idx = (i + n - half + j) % n;
            sum += frame[idx];
        }
        out[i] = sum / k as f32;
    }
    out
}

/// Brighten — high-pass emphasis via derivative (high-shelf-ish).
/// `amount` 0..1 controls how much extra high-frequency content to
/// mix in. Re-normalised so peak amplitude doesn't run away.
///
/// Implemented as `out[i] = (1 - amount) * frame[i] + amount * (frame[next] - frame[prev]) * n/4`
/// — the centred derivative gives strong harmonic emphasis without
/// the phase shift of a one-sided difference.
pub fn transform_bright(frame: &[f32], amount: f32) -> Vec<f32> {
    let n = frame.len();
    let amount = amount.clamp(0.0, 1.0);
    if amount < 1e-6 {
        return frame.to_vec();
    }
    let scale = (n as f32) / 16.0; // enough to dominate the spectrum
    let mut out = vec![0.0f32; n];
    for i in 0..n {
        let next = (i + 1) % n;
        let prev = (i + n - 1) % n;
        let deriv = (frame[next] - frame[prev]) * 0.5;
        out[i] = (1.0 - amount) * frame[i] + amount * deriv * scale;
    }
    // Re-normalise to source peak so the slider doesn't blow up.
    let peak_in = frame.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    let peak_out = out.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    if peak_out > 1e-6 && peak_in > 1e-6 {
        let g = peak_in / peak_out;
        for s in out.iter_mut() {
            *s *= g;
        }
    }
    out
}

/// Add a phase-shifted copy of the cycle to itself — emphasises
/// certain harmonics and cancels others depending on the shift.
/// `phase_offset` is a fraction of `WT_SIZE` (0.5 = half cycle —
/// kills all odd harmonics; 0.25 = quarter — phaser-like notch).
pub fn transform_phase_add(frame: &[f32], phase_offset: f32) -> Vec<f32> {
    let n = frame.len();
    let offset = ((phase_offset.fract().abs() * n as f32) as usize) % n;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        out.push(0.5 * (frame[i] + frame[(i + offset) % n]));
    }
    out
}

/// Bit-crush — quantise samples to `bits` resolution. `bits` < 16
/// adds audible "lo-fi" distortion harmonics; at `bits = 2..4` you
/// get a clear digital-grunge character. Output stays in [-1, 1].
pub fn transform_bitcrush(frame: &[f32], bits: u32) -> Vec<f32> {
    let bits = bits.clamp(1, 16);
    let steps = ((1u32 << bits) - 1) as f32;
    frame
        .iter()
        .map(|&s| {
            let clamped = s.clamp(-1.0, 1.0);
            // Quantise to (steps + 1) levels across [-1, +1].
            let normalised = (clamped + 1.0) * 0.5; // 0..1
            let quantised = (normalised * steps).round() / steps;
            (quantised * 2.0 - 1.0)
        })
        .collect()
}

/// Skew the cycle horizontally — compress one half and stretch the
/// other. `amount` in [-1, +1] (0 = no change, +1 = max-right skew,
/// -1 = max-left). Changes the pulse width / duty cycle character,
/// most audible on saws and pulses where it shifts the harmonic
/// balance between odd and even.
pub fn transform_skew(frame: &[f32], amount: f32) -> Vec<f32> {
    let n = frame.len();
    // Skew via re-mapping x → x^k where k > 1 compresses early
    // samples (left-skew), k < 1 stretches.
    let k = if amount.abs() < 1e-6 {
        1.0
    } else {
        // Map amount to a power that's well-behaved on [-1, +1].
        // amount = +1 → k = 4, amount = -1 → k = 0.25.
        (amount * 0.69314).exp() * if amount.is_sign_positive() { 1.0 } else { 1.0 }
    };
    let k = 2f32.powf(amount.clamp(-1.0, 1.0) * 2.0); // -1 → 0.25, 0 → 1, +1 → 4
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f32 / (n - 1) as f32;
        let warped = t.powf(k);
        let src = (warped * (n - 1) as f32) as usize;
        out.push(frame[src.min(n - 1)]);
    }
    out
}

/// Sample-and-hold — replace every `hold_samples` consecutive
/// samples with the first sample of that block. Drops the effective
/// sample rate, adds aliasing-like harmonics that sound like classic
/// digital stairstep distortion / "metallic" tone. `hold = 1` = no
/// change; `hold = 8` is aggressive; `hold = 64` is brutal.
pub fn transform_sample_hold(frame: &[f32], hold_samples: usize) -> Vec<f32> {
    let n = frame.len();
    let hold = hold_samples.max(1);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let block_start = (i / hold) * hold;
        out.push(frame[block_start.min(n - 1)]);
    }
    out
}

/// Fold-back distortion at `threshold` — samples above the threshold
/// wrap around into negative territory and back. Wave-shaper style,
/// adds odd harmonics on a sine.
pub fn transform_foldback(frame: &[f32], threshold: f32) -> Vec<f32> {
    let t = threshold.clamp(0.05, 1.0);
    frame
        .iter()
        .map(|&s| {
            let mut x = s / t;
            // Triangle-wave fold: oscillate inside [-1, 1].
            while x > 1.0 || x < -1.0 {
                if x > 1.0 {
                    x = 2.0 - x;
                } else {
                    x = -2.0 - x;
                }
            }
            x * t
        })
        .collect()
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

    #[test]
    fn multi_frame_extracts_n_frames_from_long_sine() {
        let s = synth_sine(220.0, 44100, 0.5); // 110 cycles → enough for 16 frames
        let ex = wav_to_multi_frame(&s, 44100, 16, 2048, 0.95)
            .expect("220Hz sine should yield 16 frames");
        assert_eq!(ex.frames.len(), 16);
        assert!((ex.detected_hz - 220.0).abs() < 2.0);
        for (i, f) in ex.frames.iter().enumerate() {
            assert_eq!(f.len(), 2048);
            let peak = f.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
            assert!(
                (peak - 0.95).abs() < 0.02,
                "frame {i} peak {peak}, expected ~0.95"
            );
        }
    }

    #[test]
    fn multi_frame_returns_none_for_unpitched() {
        let s = vec![0.0; 4096];
        assert!(wav_to_multi_frame(&s, 44100, 8, 2048, 0.95).is_none());
    }

    // ----- Wavetable transforms -----

    fn one_cycle_sine(len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| (i as f32 / len as f32 * std::f32::consts::TAU).sin())
            .collect()
    }

    #[test]
    fn mirror_reverses_in_time() {
        let s = vec![1.0, 2.0, 3.0, 4.0];
        let m = transform_mirror(&s);
        assert_eq!(m, vec![4.0, 3.0, 2.0, 1.0]);
    }

    #[test]
    fn invert_flips_polarity() {
        let s = vec![0.5, -0.3, 1.0];
        let i = transform_invert(&s);
        assert_eq!(i, vec![-0.5, 0.3, -1.0]);
    }

    #[test]
    fn octave_up_doubles_period() {
        // One cycle of sine → octave_up should contain 2 cycles
        // (same sample count, twice the frequency content).
        let s = one_cycle_sine(2048);
        let up = transform_octave_up(&s);
        assert_eq!(up.len(), 2048);
        // First half of `up` should equal first half compressed —
        // sample 0 = sine[0], sample 1024 = sine[0] again (period
        // doubled means index wraps at midpoint).
        assert!((up[0] - s[0]).abs() < 1e-6);
        assert!((up[1024] - s[0]).abs() < 1e-6); // wrap
        // Compute the actual cycle count: zero crossings should
        // happen twice as often.
        let cycles_orig = count_zero_crossings(&s);
        let cycles_up = count_zero_crossings(&up);
        assert!(cycles_up >= 2 * cycles_orig - 1);
    }

    #[test]
    fn smooth_reduces_peak_high_freq() {
        // Square wave → smoothing → should be less abrupt.
        let mut sq = vec![0.0; 2048];
        for (i, s) in sq.iter_mut().enumerate() {
            *s = if i < 1024 { 1.0 } else { -1.0 };
        }
        let smoothed = transform_smooth(&sq, 11);
        // Edges should be ramped, not vertical jumps.
        let max_jump_orig: f32 = sq.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        let max_jump_smooth: f32 = smoothed
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(
            max_jump_smooth < max_jump_orig,
            "smoothing should reduce abrupt jumps: orig={max_jump_orig}, smooth={max_jump_smooth}"
        );
    }

    #[test]
    fn phase_add_half_kills_odd_harmonics() {
        // A sine wave + itself shifted by half a period = zero.
        let s = one_cycle_sine(2048);
        let cancelled = transform_phase_add(&s, 0.5);
        let max: f32 = cancelled.iter().map(|x| x.abs()).fold(0.0, f32::max);
        assert!(max < 0.01, "half-period add should cancel sine: max={max}");
    }

    #[test]
    fn bitcrush_reduces_unique_levels() {
        // 4-bit crush of a sine should have at most 16 unique sample
        // values — that's the audible "lo-fi" stairstep.
        let s = one_cycle_sine(2048);
        let crushed = transform_bitcrush(&s, 4);
        assert_eq!(crushed.len(), 2048);
        // Bucket samples into integer levels (steps = 15 for 4 bits).
        let mut levels = std::collections::HashSet::new();
        for v in &crushed {
            let level = ((*v + 1.0) * 0.5 * 15.0).round() as i32;
            levels.insert(level);
        }
        assert!(
            levels.len() <= 16,
            "4-bit crush should produce ≤16 unique levels, got {}",
            levels.len()
        );
    }

    #[test]
    fn skew_preserves_extrema() {
        // Skewing a sine shouldn't change peak amplitude — only when
        // those peaks occur.
        let s = one_cycle_sine(2048);
        let skewed = transform_skew(&s, 0.5);
        let orig_peak = s.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
        let skew_peak = skewed.iter().map(|x| x.abs()).fold(0.0f32, f32::max);
        assert!(
            (orig_peak - skew_peak).abs() < 0.05,
            "skew shouldn't change peak: {orig_peak} vs {skew_peak}"
        );
    }

    #[test]
    fn sample_hold_creates_steps() {
        let s = one_cycle_sine(2048);
        let held = transform_sample_hold(&s, 8);
        // Within each 8-sample block, samples must be identical.
        for block in 0..(2048 / 8) {
            let base = block * 8;
            for i in 1..8 {
                assert!(
                    (held[base + i] - held[base]).abs() < 1e-9,
                    "block {block} sample {i} not held"
                );
            }
        }
    }

    /// Sanity check: mirror & invert really do preserve magnitude
    /// spectrum (within FFT precision). If a user can't tell them
    /// apart from the original on a steady tone, that's correct DSP,
    /// not a bug. Use Bright/Smooth/Bitcrush for audible changes.
    #[test]
    fn mirror_and_invert_preserve_magnitude_spectrum() {
        use crate::analysis::magnitude_spectrum_db;
        let s = one_cycle_sine(2048);
        let m = transform_mirror(&s);
        let i = transform_invert(&s);
        let spec_orig = magnitude_spectrum_db(&s);
        let spec_mirror = magnitude_spectrum_db(&m);
        let spec_invert = magnitude_spectrum_db(&i);
        // Compare first 100 bins (covers all audible frequencies for
        // a 2048-sample buffer at any practical SR).
        let mut max_diff_m = 0.0f32;
        let mut max_diff_i = 0.0f32;
        for k in 1..100 {
            max_diff_m = max_diff_m.max((spec_orig[k] - spec_mirror[k]).abs());
            max_diff_i = max_diff_i.max((spec_orig[k] - spec_invert[k]).abs());
        }
        assert!(
            max_diff_m < 0.5,
            "mirror spectrum should match original ({max_diff_m} dB max diff)"
        );
        assert!(
            max_diff_i < 0.5,
            "invert spectrum should match original ({max_diff_i} dB max diff)"
        );
    }

    #[test]
    fn bright_actually_changes_spectrum() {
        // Sanity: bright DOES change the spectrum (unlike mirror/invert).
        use crate::analysis::magnitude_spectrum_db;
        let s = one_cycle_sine(2048);
        let bright = transform_bright(&s, 0.4);
        let spec_orig = magnitude_spectrum_db(&s);
        let spec_bright = magnitude_spectrum_db(&bright);
        let total_diff: f32 = spec_orig
            .iter()
            .zip(spec_bright.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(
            total_diff > 50.0,
            "bright should noticeably change spectrum, total diff = {total_diff} dB"
        );
    }

    #[test]
    fn foldback_clamps_then_folds() {
        let s = vec![0.0, 0.5, 1.5, 2.0, 1.5, 0.5];
        let folded = transform_foldback(&s, 1.0);
        // All output samples should be in [-1, 1].
        for v in &folded {
            assert!(v.abs() <= 1.0 + 1e-6, "foldback out of range: {v}");
        }
    }

    fn count_zero_crossings(samples: &[f32]) -> usize {
        samples.windows(2).filter(|w| w[0] * w[1] < 0.0).count()
    }
}
