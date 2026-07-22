#![allow(dead_code)] // value objects — some fields are part of the model but not read by the CLI
//! Minimal value objects for the ported beat/structure detectors.
//!
//! The originals live in the `video-generator-agent` workspace and derive
//! serde / strum / schemars. sdsp-mash only needs the plain data, so these are
//! serde-free copies — nothing else changes in `beats.rs` / `structure.rs`.

/// Beat detection result.
#[derive(Debug, Clone)]
pub struct BeatResult {
    /// Beat timestamps in seconds.
    pub beats: Vec<f64>,
    /// Estimated tempo in BPM.
    pub bpm: f64,
    /// Onset energy [0,1] at each beat position. Same length as `beats`.
    pub beat_energies: Vec<f32>,
    /// Downbeat (bar start) timestamps in seconds — every `beats_per_bar`-th
    /// beat, phase-aligned by onset energy.
    pub downbeats: Vec<f64>,
    /// Detected time-signature numerator (beats per bar). Default 4 for 4/4.
    pub beats_per_bar: u8,
}

/// Configuration for beat detection.
#[derive(Debug, Clone)]
pub struct BeatConfig {
    pub n_fft: usize,
    pub hop_length: usize,
    pub min_bpm: f64,
    pub max_bpm: f64,
    pub threshold: f64,
    /// DP beat-tracker tightness (librosa default 100).
    pub tightness: f64,
    /// Trim beats in low-energy regions at start/end.
    pub trim: bool,
}

impl Default for BeatConfig {
    fn default() -> Self {
        Self {
            n_fft: 2048,
            hop_length: 512,
            min_bpm: 60.0,
            max_bpm: 200.0,
            threshold: 0.5,
            tightness: 100.0,
            trim: true,
        }
    }
}

/// A structural section of music (intro / verse / chorus / outro).
#[derive(Debug, Clone)]
pub struct AudioSection {
    pub start: f64,
    pub end: f64,
    pub label: String,
    pub novelty_score: f32,
    /// Mean RMS energy of this section [0,1].
    pub mean_energy: f32,
    /// Index of the paired section with the same label (motif repeat), if any.
    pub motif_pair: Option<usize>,
}
