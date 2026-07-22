#![allow(dead_code)] // vendored from video-generator-agent; full API kept intact
/// Beat detection — spectral flux onset detection + DP beat tracking.
///
/// Pure Rust, WASM-safe. Replaces Python librosa.beat.beat_track.
/// Algorithm: STFT → spectral flux → normalize → autocorrelation tempo → DP beat track.
///
/// The DP beat tracker (step 5) is based on librosa's approach:
/// - Gaussian-smooth onset envelope → local_score
/// - Dynamic programming: cumulative_score[t] = local_score[t] + max over previous beats
///   of (cumulative_score[p] - tightness * (ln(interval/fpb))²)
/// - Backtrack from strongest ending beat
/// - Optionally trim beats in silent regions
use realfft::RealFftPlanner;
use crate::beat_types::{BeatConfig, BeatResult};

/// Detect beats in PCM audio samples.
/// Returns beat timestamps (seconds) and estimated BPM.
pub fn detect_beats(samples: &[f32], sample_rate: u32, config: &BeatConfig) -> BeatResult {
    let empty = BeatResult {
        beats: vec![],
        bpm: 0.0,
        beat_energies: vec![],
        downbeats: vec![],
        beats_per_bar: 4,
    };
    if samples.is_empty() || sample_rate == 0 {
        return empty;
    }

    let mut planner = RealFftPlanner::<f32>::new();

    // 1. STFT → magnitude spectrogram
    let magnitudes = stft_magnitudes(samples, config.n_fft, config.hop_length, &mut planner);
    if magnitudes.len() < 2 {
        return empty;
    }

    // 2. Spectral flux (positive half-wave rectified difference)
    let flux = spectral_flux(&magnitudes);

    // Early exit: if all flux is near-zero (silence), no beats
    let max_flux = flux.iter().copied().fold(0.0f32, f32::max);
    if max_flux < 1e-6 {
        return empty;
    }

    // 3. Normalize onset strength (for meter/downbeat detection later)
    let onset = normalize_onset(&flux);

    // 4. Estimate tempo via autocorrelation
    let bpm = estimate_tempo(
        &onset,
        sample_rate,
        config.hop_length,
        config.min_bpm,
        config.max_bpm,
    );
    if bpm <= 0.0 {
        return empty;
    }

    // 5. Grid-based beat tracking: place beats on a regular grid at optimal phase.
    //    Uses raw flux (non-negative) for phase detection — more reliable than
    //    normalized onset which has negative values that confuse the DP tracker.
    let frames_per_sec = sample_rate as f64 / config.hop_length as f64;
    let beat_frames = beat_track_grid(&flux, bpm, frames_per_sec);

    // 6. Convert frames → seconds
    let mut beats: Vec<f64> = beat_frames
        .iter()
        .map(|&f| f as f64 / frames_per_sec)
        .collect();

    // 7. Optionally trim beats in low-energy regions
    if config.trim {
        beats = trim_beats(&beats, &onset, frames_per_sec);
    }

    // 8. Sample onset energy at each beat position (normalized 0..1).
    //    Use raw flux (non-negative) — normalized onset has edge artifacts
    //    that distort the accent pattern for meter detection.
    let beat_energies = sample_onset_at_beats(&beats, &flux, frames_per_sec);

    // 9. Detect meter + downbeats (bar starts) via onset energy phase alignment
    let beats_per_bar = detect_meter(&beat_energies);
    let downbeats = detect_downbeats(&beats, &beat_energies, beats_per_bar);

    BeatResult {
        beats,
        bpm,
        beat_energies,
        downbeats,
        beats_per_bar,
    }
}

/// STFT → magnitude spectrogram. Each entry is a vector of |X[k]| for one frame.
///
/// Uses center-padding (like librosa `center=True`): the signal is padded with
/// n_fft/2 zeros on each side so that frame 0 is centered at sample 0.
/// This ensures onsets at the very start of the signal are properly captured
/// (without padding, the Hann window tapers to zero at the edges, killing t=0 onsets).
pub fn stft_magnitudes(
    samples: &[f32],
    n_fft: usize,
    hop: usize,
    planner: &mut RealFftPlanner<f32>,
) -> Vec<Vec<f32>> {
    let fft = planner.plan_fft_forward(n_fft);

    // Center-pad: n_fft/2 zeros on each side (reflect-padding is also common,
    // but zero-padding is simpler and sufficient for beat detection)
    let pad = n_fft / 2;
    let mut padded = Vec::with_capacity(pad + samples.len() + pad);
    padded.resize(pad, 0.0);
    padded.extend_from_slice(samples);
    padded.resize(padded.len() + pad, 0.0);

    // Pre-compute Hann window
    let hann: Vec<f32> = (0..n_fft)
        .map(|n| 0.5 * (1.0 - (std::f32::consts::TAU * n as f32 / n_fft as f32).cos()))
        .collect();

    let mut magnitudes = Vec::new();
    let mut input = vec![0.0f32; n_fft];
    let mut spectrum = fft.make_output_vec();

    let mut offset = 0;
    while offset + n_fft <= padded.len() {
        // Apply Hann window
        for i in 0..n_fft {
            input[i] = padded[offset + i] * hann[i];
        }

        // Forward FFT (modifies input in-place as scratch)
        fft.process(&mut input, &mut spectrum)
            .expect("FFT length mismatch");

        // Magnitudes: |X[k]| = sqrt(re² + im²)
        let mags: Vec<f32> = spectrum
            .iter()
            .map(|c| (c.re * c.re + c.im * c.im).sqrt())
            .collect();
        magnitudes.push(mags);

        offset += hop;
    }

    magnitudes
}

/// Positive spectral flux: sum of positive magnitude differences per frame.
///
/// First frame compares against silence (all zeros), so an onset at t=0
/// produces a proper flux peak. Without this, the first onset is invisible.
pub fn spectral_flux(magnitudes: &[Vec<f32>]) -> Vec<f32> {
    let mut flux = Vec::with_capacity(magnitudes.len());

    if magnitudes.is_empty() {
        return flux;
    }

    // First frame: onset from silence → all magnitudes are "positive differences"
    flux.push(magnitudes[0].iter().copied().sum());

    for t in 1..magnitudes.len() {
        let sum: f32 = magnitudes[t]
            .iter()
            .zip(magnitudes[t - 1].iter())
            .map(|(&curr, &prev)| (curr - prev).max(0.0))
            .sum();
        flux.push(sum);
    }

    flux
}

/// Local window size for onset normalization (in frames).
const NORM_WINDOW: usize = 51;

/// Normalize onset strength: subtract local median, divide by local std.
fn normalize_onset(onset: &[f32]) -> Vec<f32> {
    if onset.is_empty() {
        return vec![];
    }

    let half = NORM_WINDOW / 2;
    let n = onset.len();
    let mut normalized = Vec::with_capacity(n);

    for i in 0..n {
        let start = i.saturating_sub(half);
        let end = (i + half + 1).min(n);
        let local = &onset[start..end];

        // Local median
        let mut sorted: Vec<f32> = local.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = sorted[sorted.len() / 2];

        // Local std
        let mean: f32 = local.iter().sum::<f32>() / local.len() as f32;
        let var: f32 =
            local.iter().map(|&x| (x - mean) * (x - mean)).sum::<f32>() / local.len() as f32;
        let std = var.sqrt().max(1e-10);

        normalized.push((onset[i] - median) / std);
    }

    normalized
}

/// Estimate tempo (BPM) via autocorrelation of onset strength envelope.
fn estimate_tempo(onset: &[f32], sample_rate: u32, hop: usize, min_bpm: f64, max_bpm: f64) -> f64 {
    if onset.len() < 2 {
        return 0.0;
    }

    // Convert BPM range to lag range (in onset frames)
    let frames_per_sec = sample_rate as f64 / hop as f64;
    let min_lag = (60.0 / max_bpm * frames_per_sec) as usize;
    let max_lag = ((60.0 / min_bpm * frames_per_sec) as usize).min(onset.len() - 1);

    if min_lag >= max_lag || min_lag == 0 {
        return 0.0;
    }

    // Autocorrelation with tempo prior (log-normal centered at ~120 BPM).
    // Without a prior, autocorrelation picks arbitrary periodicities.
    // librosa uses the same approach: Gaussian weighting in log-BPM space.
    let prior_center = 120.0_f64; // BPM
    let prior_sigma = 0.5_f64; // log-space spread (wider = less bias)

    // Mean autocorrelation at a given lag, and the log-normal tempo prior at
    // that lag — shared by the search, the octave resolver, and the parabolic
    // refine so all three score a lag identically.
    let autocorr_at = |lag: usize| -> f64 {
        if lag == 0 || lag >= onset.len() {
            return 0.0;
        }
        let n = onset.len() - lag;
        let mut c = 0.0f64;
        for i in 0..n {
            c += onset[i] as f64 * onset[i + lag] as f64;
        }
        c / n as f64
    };
    let prior_at = |lag: usize| -> f64 {
        let bpm = 60.0 * frames_per_sec / lag as f64;
        let log_ratio = (bpm / prior_center).ln();
        (-0.5 * (log_ratio / prior_sigma).powi(2)).exp()
    };

    let mut best_lag = min_lag;
    let mut best_score = f64::NEG_INFINITY;

    for lag in min_lag..=max_lag {
        let weighted = autocorr_at(lag) * prior_at(lag);
        if weighted > best_score {
            best_score = weighted;
            best_lag = lag;
        }
    }

    let mut best_corr = autocorr_at(best_lag);

    let dbg = std::env::var("SDSP_DEBUG_TEMPO").is_ok();
    if dbg {
        let init_bpm = 60.0 * frames_per_sec / best_lag as f64;
        eprintln!(
            "[tempo] init best_lag={best_lag} ({init_bpm:.2} bpm) corr={best_corr:.5}  \
             min_lag={min_lag} max_lag={max_lag}"
        );
    }

    // Octave resolution. The autocorrelation peak can be a harmonic (½× or 2×
    // the true beat period), so re-score the octave family {½lag, lag, 2lag}
    // with the SAME log-normal prior and keep the best. This replaces an older
    // unconditional "halve the lag whenever its corr ≥ 50% of the peak" step
    // that ignored the prior — it systematically doubled ~100 BPM tracks to
    // ~200, both clamping to the min_lag boundary and printing the identical
    // 206.72 artefact (Clubbed to Death vs Poison). The prior keeps a genuine
    // 100 BPM at 100 while still rescuing a club track misread at half tempo,
    // because the ~120-centred prior outweighs the raw-corr tie.
    {
        let mut best_weighted = best_corr * prior_at(best_lag);
        for &cand in &[best_lag / 2, best_lag * 2] {
            if cand >= min_lag && cand <= max_lag {
                let corr_c = autocorr_at(cand);
                let w = corr_c * prior_at(cand);
                if dbg {
                    let cbpm = 60.0 * frames_per_sec / cand as f64;
                    eprintln!(
                        "[tempo] octave cand lag={cand} ({cbpm:.2} bpm) corr={corr_c:.5} \
                         weighted={w:.5}  (cur weighted={best_weighted:.5})"
                    );
                }
                if w > best_weighted {
                    best_weighted = w;
                    best_lag = cand;
                    best_corr = corr_c;
                }
            }
        }
    }
    if dbg {
        let cur_bpm = 60.0 * frames_per_sec / best_lag as f64;
        eprintln!("[tempo] after octave: best_lag={best_lag} ({cur_bpm:.2} bpm)");
    }

    // Parabolic interpolation for sub-frame BPM accuracy.
    // Without this, BPM is quantized to integer lags (~3 BPM resolution at 120 BPM),
    // causing cumulative beat drift over long signals.
    let lag_precise = if best_lag > min_lag && best_lag < max_lag {
        let c_prev = autocorr_at(best_lag - 1);
        let c_next = autocorr_at(best_lag + 1);
        let denom = c_prev - 2.0 * best_corr + c_next;
        if denom.abs() > 1e-10 {
            best_lag as f64 + 0.5 * (c_prev - c_next) / denom
        } else {
            best_lag as f64
        }
    } else {
        best_lag as f64
    };

    // Convert fractional lag → BPM
    60.0 * frames_per_sec / lag_precise
}

/// Grid-based beat tracker — places beats on a regular grid at the optimal phase.
///
/// For music with constant tempo (EDM, pop, rock), this is more accurate than
/// DP tracking because:
/// 1. Beats ARE on a regular grid — no need for per-beat optimization
/// 2. Covers the entire track (no backtracking chain-break issues)
/// 3. Phase detection uses raw onset energy (no negative-value confusion)
///
/// Uses raw spectral flux (non-negative) to find the starting offset that
/// maximizes total onset energy across all grid positions.
fn beat_track_grid(flux: &[f32], bpm: f64, frames_per_sec: f64) -> Vec<usize> {
    let n = flux.len();
    if n == 0 {
        return vec![];
    }

    let fpb = 60.0 * frames_per_sec / bpm; // frames per beat
    let n_phases = fpb.ceil() as usize;

    // Gaussian window for onset energy sampling: peak at center, sigma = 2 frames.
    // This accounts for STFT resolution — a beat at frame F has energy spread
    // across F-2..F+2 due to windowing.
    let sigma = 2.0f64;
    let radius = 4usize; // 4 frames ≈ ±93ms at 22050/512

    let onset_energy_at = |frame: usize| -> f64 {
        let start = frame.saturating_sub(radius);
        let end = (frame + radius + 1).min(n);
        let mut weighted = 0.0f64;
        let mut wsum = 0.0f64;
        for (j, &val) in flux.iter().enumerate().take(end).skip(start) {
            let d = (j as f64 - frame as f64) / sigma;
            let w = (-0.5 * d * d).exp();
            weighted += val.max(0.0) as f64 * w;
            wsum += w;
        }
        if wsum > 0.0 {
            weighted / wsum
        } else {
            0.0
        }
    };

    // Search all possible starting phases (0..fpb)
    let mut best_phase = 0usize;
    let mut best_avg_energy = f64::NEG_INFINITY;

    for phase in 0..n_phases {
        let mut energy = 0.0f64;
        let mut count = 0usize;
        let mut t = phase as f64;
        while (t as usize) < n {
            energy += onset_energy_at(t.round() as usize);
            count += 1;
            t += fpb;
        }
        if count > 0 {
            let avg = energy / count as f64;
            if avg > best_avg_energy {
                best_avg_energy = avg;
                best_phase = phase;
            }
        }
    }

    // Generate regular beat grid at optimal phase
    let mut beats = Vec::new();
    let mut t = best_phase as f64;
    while (t.round() as usize) < n {
        beats.push(t.round() as usize);
        t += fpb;
    }
    beats
}

/// Sample onset energy at each beat position and normalize to [0,1].
/// Trim beats that fall in low-energy regions at the start and end.
fn trim_beats(beats: &[f64], onset: &[f32], frames_per_sec: f64) -> Vec<f64> {
    if beats.is_empty() || onset.is_empty() {
        return beats.to_vec();
    }

    // Global energy threshold: median onset strength
    let mut sorted: Vec<f32> = onset.iter().copied().filter(|&x| x > 0.0).collect();
    if sorted.is_empty() {
        return beats.to_vec();
    }
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let threshold = sorted[sorted.len() / 2] as f64 * 0.5;

    // Find first and last beat with sufficient local energy
    let onset_at_beat = |beat_time: f64| -> f64 {
        let frame = (beat_time * frames_per_sec) as usize;
        if frame < onset.len() {
            onset[frame] as f64
        } else {
            0.0
        }
    };

    let first = beats.iter().position(|&b| onset_at_beat(b) > threshold);
    let last = beats.iter().rposition(|&b| onset_at_beat(b) > threshold);

    match (first, last) {
        (Some(f), Some(l)) => beats[f..=l].to_vec(),
        _ => beats.to_vec(),
    }
}

/// Sample onset envelope at beat positions, normalize to [0..1].
fn sample_onset_at_beats(beats: &[f64], onset: &[f32], frames_per_sec: f64) -> Vec<f32> {
    if beats.is_empty() || onset.is_empty() {
        return vec![];
    }

    let raw: Vec<f32> = beats
        .iter()
        .map(|&t| {
            let frame = (t * frames_per_sec) as usize;
            if frame < onset.len() {
                onset[frame].max(0.0)
            } else {
                0.0
            }
        })
        .collect();

    let max_val = raw.iter().copied().fold(0.0f32, f32::max);
    if max_val > 1e-6 {
        raw.iter().map(|&v| v / max_val).collect()
    } else {
        vec![0.0; raw.len()]
    }
}

/// Detect downbeats (bar starts) from beat positions and their onset strengths.
///
/// Two-step algorithm:
/// 1. **Auto-detect meter** (beats per bar): try groupings 2,3,4,6 and measure
///    variance of mean onset energy across positions within each group.
///    Higher variance = stronger metric pattern = better fit. 4/4 has strong beat 1,
///    weak beats 2-4. 3/4 has strong beat 1, weak 2-3. Pick the grouping with
///    highest variance.
/// 2. **Phase alignment**: for the chosen meter, try each phase offset and pick
///    the one where downbeats have highest total onset energy.
pub fn detect_downbeats(beats: &[f64], onset_strength: &[f32], _hint: u8) -> Vec<f64> {
    if beats.len() < 4 || onset_strength.len() != beats.len() {
        return vec![];
    }

    // Step 1: detect meter by comparing onset energy variance across candidate groupings
    let candidates = [2u8, 3, 4, 6];
    let mut best_bpb = 4u8;
    let mut best_variance = f64::NEG_INFINITY;

    for &bpb in &candidates {
        let n = bpb as usize;
        if beats.len() < n * 2 {
            continue;
        } // need at least 2 bars

        // Compute mean onset energy for each position within the bar
        let mut position_sums = vec![0.0f64; n];
        let mut position_counts = vec![0usize; n];
        for (i, &s) in onset_strength.iter().enumerate() {
            let pos = i % n;
            position_sums[pos] += s as f64;
            position_counts[pos] += 1;
        }

        let means: Vec<f64> = position_sums
            .iter()
            .zip(position_counts.iter())
            .map(|(&s, &c)| if c > 0 { s / c as f64 } else { 0.0 })
            .collect();

        // Variance of position means = how much energy differs between positions
        let global_mean: f64 = means.iter().sum::<f64>() / n as f64;
        let variance: f64 = means
            .iter()
            .map(|&m| (m - global_mean) * (m - global_mean))
            .sum::<f64>()
            / n as f64;

        if variance > best_variance {
            best_variance = variance;
            best_bpb = bpb;
        }
    }

    let bpb = best_bpb as usize;

    // Step 2: find best phase (which beat position is the downbeat)
    let mut best_phase = 0;
    let mut best_energy = f64::NEG_INFINITY;

    for phase in 0..bpb {
        let energy: f64 = onset_strength
            .iter()
            .skip(phase)
            .step_by(bpb)
            .map(|&s| s as f64)
            .sum();
        if energy > best_energy {
            best_energy = energy;
            best_phase = phase;
        }
    }

    // Collect downbeats at best phase
    beats
        .iter()
        .skip(best_phase)
        .step_by(bpb)
        .copied()
        .collect()
}

/// Detect beats-per-bar from onset strength pattern. Exported for diagnostics.
pub fn detect_meter(onset_strength: &[f32]) -> u8 {
    if onset_strength.len() < 8 {
        return 4;
    }
    let candidates = [2u8, 3, 4, 6];
    let mut best_bpb = 4u8;
    let mut best_variance = f64::NEG_INFINITY;

    for &bpb in &candidates {
        let n = bpb as usize;
        if onset_strength.len() < n * 2 {
            continue;
        }

        let mut sums = vec![0.0f64; n];
        let mut counts = vec![0usize; n];
        for (i, &s) in onset_strength.iter().enumerate() {
            sums[i % n] += s as f64;
            counts[i % n] += 1;
        }
        let means: Vec<f64> = sums
            .iter()
            .zip(counts.iter())
            .map(|(&s, &c)| if c > 0 { s / c as f64 } else { 0.0 })
            .collect();
        let gm: f64 = means.iter().sum::<f64>() / n as f64;
        let var: f64 = means.iter().map(|&m| (m - gm).powi(2)).sum::<f64>() / n as f64;

        if var > best_variance {
            best_variance = var;
            best_bpb = bpb;
        }
    }
    best_bpb
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::beat_types::BeatConfig;

    /// Generate a synthetic click track: short sine bursts at regular BPM intervals.
    fn generate_click_track(bpm: f64, duration_secs: f64, sample_rate: u32) -> Vec<f32> {
        let beat_interval = 60.0 / bpm;
        let n_samples = (duration_secs * sample_rate as f64) as usize;
        let mut samples = vec![0.0f32; n_samples];

        let click_duration = 0.01; // 10ms click
        let click_samples = (click_duration * sample_rate as f64) as usize;

        let mut t = 0.0;
        while t < duration_secs {
            let start = (t * sample_rate as f64) as usize;
            for i in 0..click_samples {
                if start + i < n_samples {
                    let phase = std::f32::consts::TAU * 440.0 * i as f32 / sample_rate as f32;
                    samples[start + i] = phase.sin() * 0.8;
                }
            }
            t += beat_interval;
        }

        samples
    }

    /// Click track with a strong click on each beat and a weaker "ghost" click
    /// on every off-beat (eighth). The off-beat energy is what makes the
    /// half-lag (double-tempo) autocorrelation strong — the exact condition that
    /// used to make the octave step wrongly double a ~100 BPM track to ~200.
    fn generate_click_track_with_ghosts(
        bpm: f64,
        ghost_amp: f32,
        duration_secs: f64,
        sample_rate: u32,
    ) -> Vec<f32> {
        let half_interval = 30.0 / bpm; // eighth-note spacing
        let n_samples = (duration_secs * sample_rate as f64) as usize;
        let mut samples = vec![0.0f32; n_samples];
        let click_samples = (0.01 * sample_rate as f64) as usize;

        let mut k = 0usize;
        let mut t = 0.0;
        while t < duration_secs {
            let amp = if k % 2 == 0 { 0.9 } else { ghost_amp };
            let start = (t * sample_rate as f64) as usize;
            for i in 0..click_samples {
                if start + i < n_samples {
                    let phase = std::f32::consts::TAU * 440.0 * i as f32 / sample_rate as f32;
                    samples[start + i] = phase.sin() * amp;
                }
            }
            t += half_interval;
            k += 1;
        }
        samples
    }

    /// Regression for the 206.72 octave-doubling bug: a ~103 BPM groove with
    /// audible off-beats must NOT be reported as ~206. Before the prior-weighted
    /// octave fix, the old "halve the lag if corr ≥ 50%" step doubled the tempo,
    /// and the doubled lag clamped to `min_lag` → a constant 206.72 for every
    /// track near 103 BPM (Clubbed to Death and Poison came out identical).
    #[test]
    fn test_ghost_offbeats_not_doubled() {
        let sr = 22050u32;
        for &bpm in &[100.0, 103.0, 108.0] {
            // Ghost = 65% of the downbeat — strong enough that the half-lag
            // autocorrelation is > 50% of the beat-lag (the old trigger).
            let samples = generate_click_track_with_ghosts(bpm, 0.65, 12.0, sr);
            let result = detect_beats(&samples, sr, &BeatConfig::default());
            assert!(
                (result.bpm - bpm).abs() < 6.0,
                "expected ~{bpm} BPM, got {:.2} (octave-doubling regression?)",
                result.bpm
            );
            assert!(
                result.bpm < 150.0,
                "tempo {:.2} looks octave-doubled from {bpm}",
                result.bpm
            );
        }
    }

    #[test]
    fn test_silent_audio_returns_empty() {
        let samples = vec![0.0f32; 22050]; // 1 second of silence
        let result = detect_beats(&samples, 22050, &BeatConfig::default());
        assert!(result.beats.is_empty());
    }

    #[test]
    fn test_empty_audio_returns_empty() {
        let result = detect_beats(&[], 22050, &BeatConfig::default());
        assert!(result.beats.is_empty());
        assert_eq!(result.bpm, 0.0);
    }

    #[test]
    fn test_zero_sample_rate_returns_empty() {
        let samples = vec![0.5f32; 1000];
        let result = detect_beats(&samples, 0, &BeatConfig::default());
        assert!(result.beats.is_empty());
    }

    #[test]
    fn test_single_click() {
        let sr = 22050u32;
        let mut samples = vec![0.0f32; sr as usize * 2]; // 2 seconds
                                                         // One click at t=0.5s
        let start = (0.5 * sr as f64) as usize;
        for i in 0..220 {
            if start + i < samples.len() {
                let phase = std::f32::consts::TAU * 440.0 * i as f32 / sr as f32;
                samples[start + i] = phase.sin() * 0.8;
            }
        }

        let result = detect_beats(&samples, sr, &BeatConfig::default());
        // Should detect at least one beat
        assert!(
            !result.beats.is_empty(),
            "expected at least one beat from single click"
        );
    }

    #[test]
    fn test_120bpm_detection() {
        let sr = 22050u32;
        let bpm = 120.0;
        let duration = 5.0;
        let samples = generate_click_track(bpm, duration, sr);

        let result = detect_beats(&samples, sr, &BeatConfig::default());

        // BPM should be within ±5 of actual
        assert!(
            (result.bpm - bpm).abs() < 5.0,
            "expected BPM ~{bpm}, got {:.1}",
            result.bpm
        );

        // Should have roughly the right number of beats
        let expected_beats = (duration * bpm / 60.0) as usize;
        assert!(
            result.beats.len() >= expected_beats / 2,
            "expected ~{expected_beats} beats, got {}",
            result.beats.len()
        );
    }

    #[test]
    fn test_beat_absolute_positions() {
        let sr = 22050u32;
        let bpm = 120.0;
        let duration = 8.0;
        let samples = generate_click_track(bpm, duration, sr);

        let result = detect_beats(&samples, sr, &BeatConfig::default());
        let beat_interval = 60.0 / bpm; // 0.5s

        eprintln!(
            "BPM: {:.1}, {} beats detected",
            result.bpm,
            result.beats.len()
        );
        eprintln!("First 10 beats (expected vs detected):");
        for (i, &beat) in result.beats.iter().take(10).enumerate() {
            let expected = i as f64 * beat_interval;
            let error = beat - expected;
            eprintln!(
                "  beat[{i}]: expected={expected:.4}, detected={beat:.4}, error={error:+.4}s"
            );
        }

        // First beat should be within 1 beat period of 0.0
        assert!(
            result.beats[0] < beat_interval,
            "first beat at {:.3}s — should be near 0.0, not {:.1} beats late",
            result.beats[0],
            result.beats[0] / beat_interval
        );
    }

    #[test]
    fn test_80bpm_detection() {
        let sr = 22050u32;
        let bpm = 80.0;
        let duration = 6.0;
        let samples = generate_click_track(bpm, duration, sr);

        let result = detect_beats(&samples, sr, &BeatConfig::default());
        assert!(
            (result.bpm - bpm).abs() < 5.0,
            "expected BPM ~{bpm}, got {:.1}",
            result.bpm
        );
    }

    #[test]
    fn test_160bpm_detection() {
        let sr = 22050u32;
        let bpm = 160.0;
        let duration = 5.0;
        let samples = generate_click_track(bpm, duration, sr);

        let result = detect_beats(&samples, sr, &BeatConfig::default());
        assert!(
            (result.bpm - bpm).abs() < 5.0,
            "expected BPM ~{bpm}, got {:.1}",
            result.bpm
        );
    }

    #[test]
    fn test_beat_intervals_consistent() {
        let sr = 22050u32;
        let bpm = 120.0;
        let duration = 4.0;
        let samples = generate_click_track(bpm, duration, sr);

        let result = detect_beats(&samples, sr, &BeatConfig::default());
        let expected_interval = 60.0 / bpm;

        assert!(
            result.beats.len() >= 4,
            "expected ≥4 beats, got {}",
            result.beats.len()
        );

        // DP tracker optimizes beat placement — check inter-beat intervals are close to expected.
        // Allow ±100ms tolerance (hop=512/22050 ≈ 23ms resolution, DP may shift by a few frames).
        for window in result.beats.windows(2) {
            let interval = window[1] - window[0];
            let error = (interval - expected_interval).abs();
            assert!(
                error < 0.10,
                "interval {interval:.3}s deviates {error:.3}s from expected {expected_interval:.3}s"
            );
        }
    }

    #[test]
    fn test_stft_magnitudes_basic() {
        let sr = 22050u32;
        let n_fft = 2048;
        let hop = 512;
        // Generate 1 second of 440Hz sine
        let samples: Vec<f32> = (0..sr)
            .map(|i| (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin())
            .collect();

        let mut planner = RealFftPlanner::<f32>::new();
        let mags = stft_magnitudes(&samples, n_fft, hop, &mut planner);

        assert!(!mags.is_empty());
        // Each magnitude vector should have n_fft/2 + 1 bins
        assert_eq!(mags[0].len(), n_fft / 2 + 1);
    }

    /// Generate click track with accented beat 1 (downbeat louder than beats 2-4).
    fn generate_accented_click_track(
        bpm: f64,
        duration_secs: f64,
        sample_rate: u32,
        beats_per_bar: usize,
    ) -> Vec<f32> {
        let beat_interval = 60.0 / bpm;
        let n_samples = (duration_secs * sample_rate as f64) as usize;
        let mut samples = vec![0.0f32; n_samples];
        let click_duration = 0.01;
        let click_samples = (click_duration * sample_rate as f64) as usize;

        let mut t = 0.0;
        let mut beat_idx = 0;
        while t < duration_secs {
            let amp = if beat_idx % beats_per_bar == 0 {
                1.0
            } else {
                0.3
            };
            let start = (t * sample_rate as f64) as usize;
            for i in 0..click_samples {
                if start + i < n_samples {
                    let phase = std::f32::consts::TAU * 440.0 * i as f32 / sample_rate as f32;
                    samples[start + i] = phase.sin() * amp;
                }
            }
            t += beat_interval;
            beat_idx += 1;
        }
        samples
    }

    #[test]
    fn test_downbeat_detection_4_4() {
        let sr = 22050u32;
        let bpm = 120.0;
        let duration = 8.0; // 4 bars of 4/4
        let samples = generate_accented_click_track(bpm, duration, sr, 4);

        let result = detect_beats(&samples, sr, &BeatConfig::default());

        assert!(!result.downbeats.is_empty(), "should detect downbeats");
        assert!(
            result.beats_per_bar == 4 || result.beats_per_bar == 2,
            "meter should be 4 or 2, got {}",
            result.beats_per_bar
        );

        // Downbeat intervals should be multiples of beat interval
        let bar_dur = 60.0 / bpm * result.beats_per_bar as f64;
        for w in result.downbeats.windows(2) {
            let interval = w[1] - w[0];
            let error = (interval - bar_dur).abs();
            assert!(
                error < 0.15,
                "downbeat interval {interval:.3}s, expected {bar_dur:.3}s"
            );
        }
    }

    #[test]
    fn test_detect_meter_accented_4_4() {
        // Simulate onset pattern: strong on beat 1, weak on 2-4
        let onset: Vec<f32> = (0..40)
            .map(|i| if i % 4 == 0 { 1.0 } else { 0.3 })
            .collect();
        let meter = detect_meter(&onset);
        assert_eq!(meter, 4, "should detect 4/4 meter");
    }

    #[test]
    fn test_detect_meter_accented_3_4() {
        // Waltz: strong on beat 1, weak on 2-3
        let onset: Vec<f32> = (0..30)
            .map(|i| if i % 3 == 0 { 1.0 } else { 0.2 })
            .collect();
        let meter = detect_meter(&onset);
        assert_eq!(meter, 3, "should detect 3/4 meter");
    }

    #[test]
    fn test_downbeat_phase_alignment() {
        // 8 beats, 4/4 time. Beat 1 (idx 0,4) is strong.
        let beats = vec![0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5];
        let onset = vec![1.0, 0.2, 0.3, 0.2, 0.9, 0.2, 0.3, 0.2]; // strong on 0, 4
        let db = detect_downbeats(&beats, &onset, 4);
        assert_eq!(db, vec![0.0, 2.0], "downbeats should be at bar starts");
    }

    #[test]
    fn test_downbeat_phase_offset() {
        // Same but strong beats shifted by 1 (phase=1)
        let beats = vec![0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5];
        let onset = vec![0.2, 1.0, 0.2, 0.3, 0.2, 0.9, 0.2, 0.3]; // strong on 1, 5
        let db = detect_downbeats(&beats, &onset, 4);
        assert_eq!(db, vec![0.5, 2.5], "downbeats should follow energy phase");
    }

    /// Generate a click track with a silence gap in the middle.
    /// Pattern: clicks for `active_secs` → silence for `gap_secs` → clicks for `active_secs`.
    fn generate_click_track_with_gap(
        bpm: f64,
        active_secs: f64,
        gap_secs: f64,
        sample_rate: u32,
    ) -> Vec<f32> {
        let beat_interval = 60.0 / bpm;
        let total_secs = active_secs * 2.0 + gap_secs;
        let n_samples = (total_secs * sample_rate as f64) as usize;
        let mut samples = vec![0.0f32; n_samples];

        let click_duration = 0.01;
        let click_samples = (click_duration * sample_rate as f64) as usize;
        let gap_start = active_secs;
        let gap_end = active_secs + gap_secs;

        let mut t = 0.0;
        while t < total_secs {
            // Skip clicks during the gap
            if t >= gap_start && t < gap_end {
                t += beat_interval;
                continue;
            }
            let start = (t * sample_rate as f64) as usize;
            for i in 0..click_samples {
                if start + i < n_samples {
                    let phase = std::f32::consts::TAU * 440.0 * i as f32 / sample_rate as f32;
                    samples[start + i] = phase.sin() * 0.8;
                }
            }
            t += beat_interval;
        }
        samples
    }

    #[test]
    fn test_silence_gap_in_middle() {
        // Realistic scenario: music → 2s breakdown/pause → music resumes.
        // Tests that BPM estimation works despite discontinuity and beat grid
        // spans the full signal including the silent gap.
        let sr = 22050u32;
        let bpm = 120.0;
        let samples = generate_click_track_with_gap(bpm, 3.0, 2.0, sr);

        let result = detect_beats(&samples, sr, &BeatConfig::default());

        // BPM should still be detected correctly — enough click material exists
        assert!(
            result.bpm > 0.0,
            "should detect tempo despite silence gap"
        );
        assert!(
            (result.bpm - bpm).abs() < 8.0,
            "expected BPM ~{bpm} despite gap, got {:.1}",
            result.bpm
        );

        // Should detect beats in both active sections
        assert!(
            result.beats.len() >= 4,
            "expected beats across both active sections, got {}",
            result.beats.len()
        );

        // Beats should span both sides of the gap (before 3s and after 5s)
        let has_before_gap = result.beats.iter().any(|&b| b < 3.0);
        let has_after_gap = result.beats.iter().any(|&b| b > 5.0);
        assert!(has_before_gap, "should have beats before the gap");
        assert!(has_after_gap, "should have beats after the gap");

        // Monotonicity still holds
        for w in result.beats.windows(2) {
            assert!(w[1] > w[0], "beats not monotonic across gap: {} >= {}", w[0], w[1]);
        }

        // beat_energies length must match beats
        assert_eq!(result.beat_energies.len(), result.beats.len());
    }

    #[test]
    fn test_spectral_flux_nonnegative() {
        let mags = vec![
            vec![0.0, 1.0, 0.5],
            vec![0.0, 2.0, 0.3], // flux: max(0, 0) + max(0, 1.0) + max(0, -0.2) = 1.0
            vec![0.0, 0.5, 0.1], // flux: all negative = 0
        ];
        let flux = spectral_flux(&mags);
        assert_eq!(flux.len(), 3);
        // First frame: sum of magnitudes (onset from silence) = 0.0 + 1.0 + 0.5 = 1.5
        assert_eq!(flux[0], 1.5);
        assert!(flux[1] >= 0.0);
        assert!(flux[2] >= 0.0);
    }
}
