#![allow(dead_code)] // vendored from video-generator-agent; full API kept intact
use crate::beat_types::AudioSection;

/// Detect music structure via novelty-based segmentation.
///
/// Feature vectors per STFT frame: [energy, spectral_centroid, flux] (3D).
/// Self-similarity → checkerboard kernel convolution → novelty peaks → sections.
pub fn detect_structure(
    magnitudes: &[Vec<f32>],
    sample_rate: u32,
    hop_length: usize,
    kernel_size: usize,
    min_section_sec: f64,
) -> Vec<AudioSection> {
    if magnitudes.len() < 4 {
        return vec![];
    }

    let frames_per_sec = sample_rate as f64 / hop_length as f64;

    // 1. Extract per-frame features: [energy, centroid, flux]
    let features = extract_frame_features(magnitudes, sample_rate, hop_length);
    let n = features.len();
    if n < 4 {
        return vec![];
    }

    // 2. Compute novelty curve via checkerboard kernel on self-similarity
    let novelty = compute_novelty_curve(&features, kernel_size);

    // 3. Pick peaks in novelty curve
    let min_gap_frames = (min_section_sec * frames_per_sec) as usize;
    let boundaries = pick_peaks(&novelty, min_gap_frames);

    // 4. Convert boundaries to sections with labels
    let duration = n as f64 / frames_per_sec;
    label_sections(&boundaries, &features, frames_per_sec, duration)
}

/// Per-frame feature vector: [energy, spectral_centroid, flux].
pub fn extract_frame_features(
    magnitudes: &[Vec<f32>],
    sample_rate: u32,
    _hop_length: usize,
) -> Vec<[f32; 3]> {
    let n = magnitudes.len();
    let mut features = Vec::with_capacity(n);

    for t in 0..n {
        let frame = &magnitudes[t];

        // Energy: sum of squared magnitudes
        let energy: f32 = frame.iter().map(|&m| m * m).sum();

        // Spectral centroid (normalized by Nyquist)
        let nyquist = sample_rate as f32 / 2.0;
        let centroid = if frame.len() > 1 && nyquist > 0.0 {
            let mut weighted_sum = 0.0f64;
            let mut mag_sum = 0.0f64;
            for (k, &m) in frame.iter().enumerate().skip(1) {
                let freq = k as f64 * sample_rate as f64 / (2 * (frame.len() - 1)) as f64;
                weighted_sum += freq * m as f64;
                mag_sum += m as f64;
            }
            if mag_sum > 1e-10 {
                (weighted_sum / mag_sum / nyquist as f64) as f32
            } else {
                0.0
            }
        } else {
            0.0
        };

        // Spectral flux (positive difference from previous frame)
        let flux = if t > 0 {
            magnitudes[t]
                .iter()
                .zip(magnitudes[t - 1].iter())
                .map(|(&curr, &prev)| (curr - prev).max(0.0))
                .sum()
        } else {
            0.0
        };

        features.push([energy, centroid, flux]);
    }

    features
}

/// Compute novelty curve via checkerboard kernel on cosine self-similarity.
pub fn compute_novelty_curve(features: &[[f32; 3]], kernel_size: usize) -> Vec<f32> {
    let n = features.len();
    let half_k = kernel_size / 2;
    let mut novelty = vec![0.0f32; n];

    for (t, nov) in novelty
        .iter_mut()
        .enumerate()
        .take(n.saturating_sub(half_k))
        .skip(half_k)
    {
        // Checkerboard kernel: compare features before vs after boundary at t
        let mut cross_sim = 0.0f64;
        let mut count = 0u32;

        let range = half_k.min(t).min(n - t);
        for i in 1..=range {
            let before = t.checked_sub(i);
            let after = if t + i < n { Some(t + i) } else { None };

            if let (Some(b), Some(a)) = (before, after) {
                let sim = cosine_sim(&features[b], &features[a]);
                cross_sim += sim as f64;
                count += 1;
            }
        }

        if count > 0 {
            // Novelty = 1 - mean cross-block similarity (high novelty = different blocks)
            *nov = (1.0 - cross_sim / count as f64).max(0.0) as f32;
        }
    }

    novelty
}

/// Cosine similarity between two 3D feature vectors.
pub fn cosine_sim(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(&x, &y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|&x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|&x| x * x).sum::<f32>().sqrt();
    if norm_a > 1e-10 && norm_b > 1e-10 {
        dot / (norm_a * norm_b)
    } else {
        0.0
    }
}

/// Pick peaks in novelty curve with minimum gap between peaks.
pub fn pick_peaks(novelty: &[f32], min_gap: usize) -> Vec<usize> {
    if novelty.is_empty() {
        return vec![];
    }

    // Threshold: mean + 1 std
    let mean = novelty.iter().sum::<f32>() / novelty.len() as f32;
    let variance = novelty
        .iter()
        .map(|&x| (x - mean) * (x - mean))
        .sum::<f32>()
        / novelty.len() as f32;
    let threshold = mean + variance.sqrt();

    let mut peaks: Vec<(usize, f32)> = Vec::new();

    for i in 1..novelty.len().saturating_sub(1) {
        if novelty[i] > threshold && novelty[i] >= novelty[i - 1] && novelty[i] >= novelty[i + 1] {
            peaks.push((i, novelty[i]));
        }
    }

    // Sort by strength descending, then greedily select with min gap
    peaks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut selected: Vec<usize> = Vec::new();
    for (pos, _) in peaks {
        if selected.iter().all(|&s| {
            let diff = pos.abs_diff(s);
            diff >= min_gap
        }) {
            selected.push(pos);
        }
    }

    selected.sort();
    selected
}

/// Label sections based on energy patterns.
pub fn label_sections(
    boundaries: &[usize],
    features: &[[f32; 3]],
    frames_per_sec: f64,
    duration: f64,
) -> Vec<AudioSection> {
    if features.is_empty() {
        return vec![];
    }

    // Build section boundaries: [0, b1, b2, ..., n]
    let mut bounds: Vec<usize> = vec![0];
    bounds.extend_from_slice(boundaries);
    bounds.push(features.len());
    bounds.dedup();

    if bounds.len() < 2 {
        return vec![AudioSection {
            start: 0.0,
            end: duration,
            label: "section".to_string(),
            novelty_score: 0.0,
            mean_energy: 0.0,
            motif_pair: None,
        }];
    }

    // Compute mean energy per section
    let mut sections: Vec<(f64, f64, f32)> = Vec::new(); // (start, end, mean_energy)
    for w in bounds.windows(2) {
        let s = w[0];
        let e = w[1];
        let start_sec = s as f64 / frames_per_sec;
        let end_sec = (e as f64 / frames_per_sec).min(duration);
        let mean_energy = if e > s {
            features[s..e].iter().map(|f| f[0]).sum::<f32>() / (e - s) as f32
        } else {
            0.0
        };
        sections.push((start_sec, end_sec, mean_energy));
    }

    if sections.is_empty() {
        return vec![];
    }

    // Find energy thresholds for labeling
    let energies: Vec<f32> = sections.iter().map(|s| s.2).collect();
    let max_energy = energies.iter().copied().fold(0.0f32, f32::max);
    let low_threshold = max_energy * 0.3;

    // Label: first low-energy = intro, last low-energy = outro,
    // highest energy = chorus, rest = verse
    let n_sections = sections.len();
    let mut labels: Vec<String> = vec!["verse".to_string(); n_sections];

    // Find highest energy section(s) → chorus
    if max_energy > 0.0 {
        let chorus_threshold = max_energy * 0.8;
        for (i, &(_, _, e)) in sections.iter().enumerate() {
            if e >= chorus_threshold {
                labels[i] = "chorus".to_string();
            }
        }
    }

    // First section: intro if low energy
    if sections[0].2 < low_threshold {
        labels[0] = "intro".to_string();
    }

    // Last section: outro if low energy
    if n_sections > 1 && sections[n_sections - 1].2 < low_threshold {
        labels[n_sections - 1] = "outro".to_string();
    }

    let mut result: Vec<AudioSection> = sections
        .iter()
        .zip(labels.iter())
        .map(|(&(start, end, energy), label)| {
            // Normalize mean_energy to [0,1] relative to max
            let norm_energy = if max_energy > 0.0 {
                energy / max_energy
            } else {
                0.0
            };
            AudioSection {
                start,
                end,
                label: label.clone(),
                novelty_score: 0.0,
                mean_energy: norm_energy,
                motif_pair: None,
            }
        })
        .collect();

    detect_motif_pairs(&mut result);
    result
}

/// Detect motif pairs: sections with the same label that are musically similar.
///
/// For each pair of sections with the same label, mark `motif_pair` if their
/// energy levels are within a similarity threshold (cosine-like).
pub fn detect_motif_pairs(sections: &mut [AudioSection]) {
    if sections.len() < 2 {
        return;
    }
    // Group section indices by label
    let mut label_groups: std::collections::HashMap<String, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, sec) in sections.iter().enumerate() {
        label_groups.entry(sec.label.clone()).or_default().push(i);
    }
    // For each group with 2+ members, pair them by closest energy
    for indices in label_groups.values() {
        if indices.len() < 2 {
            continue;
        }
        for i in 0..indices.len() {
            let mut best_j = None;
            let mut best_diff = f32::MAX;
            for j in 0..indices.len() {
                if i == j {
                    continue;
                }
                let diff =
                    (sections[indices[i]].mean_energy - sections[indices[j]].mean_energy).abs();
                if diff < best_diff {
                    best_diff = diff;
                    best_j = Some(indices[j]);
                }
            }
            if best_diff < 0.3 {
                sections[indices[i]].motif_pair = best_j;
            }
        }
    }
}

/// Find which section a given time falls in. Returns section index.
pub fn section_at_time(sections: &[AudioSection], time: f64) -> Option<usize> {
    sections
        .iter()
        .position(|s| time >= s.start && time < s.end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_sim_identical_is_one() {
        let a = [1.0, 2.0, 3.0];
        assert!((cosine_sim(&a, &a) - 1.0).abs() < 1e-5);
        // Orthogonal-ish vectors score low.
        let b = [3.0, 0.0, 0.0];
        let c = [0.0, 3.0, 0.0];
        assert!(cosine_sim(&b, &c).abs() < 1e-5);
    }

    #[test]
    fn section_at_time_locates_the_section() {
        let secs = vec![
            AudioSection { start: 0.0, end: 10.0, label: "intro".into(), novelty_score: 0.0, mean_energy: 0.1, motif_pair: None },
            AudioSection { start: 10.0, end: 30.0, label: "verse".into(), novelty_score: 0.0, mean_energy: 0.5, motif_pair: None },
            AudioSection { start: 30.0, end: 60.0, label: "chorus".into(), novelty_score: 0.0, mean_energy: 0.9, motif_pair: None },
        ];
        assert_eq!(section_at_time(&secs, 5.0), Some(0));
        assert_eq!(section_at_time(&secs, 15.0), Some(1));
        assert_eq!(section_at_time(&secs, 45.0), Some(2));
        assert_eq!(section_at_time(&secs, 100.0), None);
    }

    #[test]
    fn detect_structure_segments_energy_contrast() {
        // Two blocks: quiet then loud → at least one boundary → 2+ sections.
        let hop = 512usize;
        let sr = 44_100u32;
        let mut mags: Vec<Vec<f32>> = Vec::new();
        for t in 0..200 {
            let e = if t < 100 { 0.05 } else { 0.9 };
            mags.push((0..64).map(|k| e * (1.0 + (k as f32 * 0.01))).collect());
        }
        let secs = detect_structure(&mags, sr, hop, 32, 0.1);
        assert!(secs.len() >= 2, "expected a boundary, got {} sections", secs.len());
    }
}
