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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveExtra {
    /// The drawn high-resolution wavetable. Always `WT_SIZE` samples.
    pub frame_a: Vec<f32>,
}

impl PresetExtra for WaveExtra {
    fn validate(&self) -> Result<(), PresetError> {
        if self.frame_a.len() != WT_SIZE {
            return Err(PresetError::ExtraInvalid(format!(
                "frame_a length {} (expected {WT_SIZE})",
                self.frame_a.len()
            )));
        }
        if self.frame_a.iter().any(|s| !s.is_finite()) {
            return Err(PresetError::ExtraInvalid(
                "frame_a contains non-finite sample (NaN / Inf)".into(),
            ));
        }
        if self.frame_a.iter().any(|s| s.abs() > 2.0) {
            return Err(PresetError::ExtraInvalid(
                "frame_a sample magnitude > 2.0 (file likely corrupt)".into(),
            ));
        }
        Ok(())
    }
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
