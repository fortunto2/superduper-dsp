//! Plugin-specific extra payload for SuperDuper Wave user presets.
//!
//! What we store beyond params:
//! - `frame_a`: the high-resolution drawn wavetable (`WT_SIZE` floats).
//!
//! Validation invariants:
//! - `frame_a.len() == WT_SIZE` — otherwise the mip-pyramid rebuild
//!   would either truncate or panic on `osc::mip_from_table`.
//! - Every sample must be finite. NaN/Inf at audio rate destroys the
//!   filter integrators and the bug is sticky-silent (no panic, just
//!   no sound).
//! - Allow slight overshoot of ±2.0 — user might draw out of normal
//!   bounds and the soft-clip downstream copes — but reject anything
//!   wildly off (corrupted file, missing decimal point).

use serde::{Deserialize, Serialize};
use superduper_synth_core::user_preset::{PresetError, PresetExtra};

use crate::osc::WT_SIZE;
use crate::FRAMES_MAX;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveExtra {
    /// Legacy single-cycle frame field — kept for backward compat
    /// with v1 presets that pre-date the multi-frame array. New
    /// saves still write this (set to frames[0]) so older builds
    /// can still load the patch as a single-cycle wavetable.
    pub frame_a: Vec<f32>,
    /// Full multi-frame wavetable (1..=`FRAMES_MAX` entries). Empty
    /// for v1 presets — `effective_frames()` does the fallback.
    /// `WT Pos × (N-1)` morphs across them.
    #[serde(default)]
    pub frames: Vec<Vec<f32>>,
}

impl WaveExtra {
    /// Construct from a list of frames. Always populates `frame_a`
    /// from frames[0] so v1-build loaders see a valid single cycle.
    pub fn from_frames(frames: Vec<Vec<f32>>) -> Self {
        let frame_a = frames.first().cloned().unwrap_or_default();
        Self { frame_a, frames }
    }

    /// Return the active frame set — uses `frames` if non-empty,
    /// otherwise falls back to `vec![frame_a]` for backward compat.
    /// Always at least 1 entry on a validated WaveExtra.
    pub fn effective_frames(&self) -> Vec<Vec<f32>> {
        if !self.frames.is_empty() {
            self.frames.clone()
        } else {
            vec![self.frame_a.clone()]
        }
    }
}

impl PresetExtra for WaveExtra {
    fn validate(&self) -> Result<(), PresetError> {
        // frame_a must always be a valid single-cycle (legacy
        // contract).
        validate_frame(&self.frame_a, "frame_a")?;
        // frames (if provided) must each be valid + length cap.
        if self.frames.len() > FRAMES_MAX {
            return Err(PresetError::ExtraInvalid(format!(
                "frames count {} exceeds FRAMES_MAX={FRAMES_MAX}",
                self.frames.len()
            )));
        }
        for (i, f) in self.frames.iter().enumerate() {
            validate_frame(f, &format!("frames[{i}]"))?;
        }
        Ok(())
    }
}

/// Single-frame validation shared between `frame_a` and the
/// `frames` array entries — length match, finite samples, range
/// sanity. Catches corrupted JSON the same way for either path.
fn validate_frame(frame: &[f32], label: &str) -> Result<(), PresetError> {
    if frame.len() != WT_SIZE {
        return Err(PresetError::ExtraInvalid(format!(
            "{label} length {} (expected {WT_SIZE})",
            frame.len()
        )));
    }
    if frame.iter().any(|s| !s.is_finite()) {
        return Err(PresetError::ExtraInvalid(format!(
            "{label} contains non-finite sample (NaN / Inf)"
        )));
    }
    if frame.iter().any(|s| s.abs() > 2.0) {
        return Err(PresetError::ExtraInvalid(format!(
            "{label} sample magnitude > 2.0 (file likely corrupt)"
        )));
    }
    Ok(())
}

pub type WavePreset = superduper_synth_core::user_preset::UserPreset<WaveExtra>;
pub type WaveRepo = superduper_synth_core::user_preset::PresetRepo<WaveExtra>;

/// One repo instance lives in a `OnceLock` so every GUI window plus the
/// audio-thread default loader share the same `~/.superduper-dsp/wave`
/// path resolution without re-walking `HOME`.
pub fn repo() -> &'static WaveRepo {
    use std::sync::OnceLock;
    static REPO: OnceLock<WaveRepo> = OnceLock::new();
    REPO.get_or_init(|| WaveRepo::for_plugin("wave"))
}
