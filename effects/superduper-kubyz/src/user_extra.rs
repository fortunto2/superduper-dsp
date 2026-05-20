//! Plugin-specific extra payload for SuperDuper Kubyz user presets.
//!
//! Beyond params, Kubyz stores:
//! - 16 harmonic amplitudes (the additive engine's harmonic spectrum)
//! - 3 formant bandwidths + 3 gains (the vowel-pad filter bank)
//!
//! Validation enforces array lengths, finite floats, and that formant
//! bandwidths stay positive — a zero bandwidth divides by zero in the
//! biquad coefficient formula.

use serde::{Deserialize, Serialize};
use superduper_synth_core::user_preset::{PresetError, PresetExtra};

use crate::presets::N_HARMONICS;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KubyzExtra {
    /// Linear amplitudes for the 16 harmonics. Harmonic 1 is the
    /// fundamental — usually normalised to 1.0, others 0..1.
    pub harmonics: Vec<f32>,
    /// Centre-frequency offsets are PARAMS-bound (P_F1/F2/F3); here we
    /// persist the per-formant bandwidth (Hz) and gain (linear).
    pub formant_bw: [f32; 3],
    pub formant_gain: [f32; 3],
}

impl PresetExtra for KubyzExtra {
    fn validate(&self) -> Result<(), PresetError> {
        if self.harmonics.len() != N_HARMONICS {
            return Err(PresetError::ExtraInvalid(format!(
                "harmonics length {} (expected {N_HARMONICS})",
                self.harmonics.len()
            )));
        }
        if self.harmonics.iter().any(|h| !h.is_finite()) {
            return Err(PresetError::ExtraInvalid(
                "harmonics contain non-finite value".into(),
            ));
        }
        if self.formant_bw.iter().any(|bw| !bw.is_finite() || *bw <= 0.0) {
            return Err(PresetError::ExtraInvalid(
                "formant_bw must be finite and > 0 Hz".into(),
            ));
        }
        if self.formant_gain.iter().any(|g| !g.is_finite()) {
            return Err(PresetError::ExtraInvalid(
                "formant_gain contains non-finite value".into(),
            ));
        }
        Ok(())
    }
}

pub type KubyzUserPreset = superduper_synth_core::user_preset::UserPreset<KubyzExtra>;
pub type KubyzRepo = superduper_synth_core::user_preset::PresetRepo<KubyzExtra>;

pub fn repo() -> &'static KubyzRepo {
    use std::sync::OnceLock;
    static REPO: OnceLock<KubyzRepo> = OnceLock::new();
    REPO.get_or_init(|| KubyzRepo::for_plugin("kubyz"))
}
