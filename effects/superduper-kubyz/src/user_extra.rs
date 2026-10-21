//! Plugin-specific extra payload for SuperDuper Kubyz user presets.
//!
//! Beyond params, Kubyz stores:
//! - 16 harmonic amplitudes (the additive engine's harmonic spectrum)
//! - 3 formant bandwidths + 3 gains (the vowel-pad filter bank)
//! - the live formant count + bands 4–6 the formant editor adds (both
//!   optional: presets saved before the editor load as 3-band)
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
    /// Live formant bands 1..=6 (defaults to 3 for older files).
    #[serde(default = "crate::formants::default_count")]
    pub formant_count: u32,
    /// Bands 4–6 — centre / bandwidth / gain. Empty in older files.
    #[serde(default)]
    pub formant_free: Vec<crate::formants::Band>,
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
        let n = self.formant_count as usize;
        if !(crate::formants::MIN_FORMANTS..=crate::formants::MAX_FORMANTS).contains(&n) {
            return Err(PresetError::ExtraInvalid(format!(
                "formant_count {n} (expected 1..={})",
                crate::formants::MAX_FORMANTS
            )));
        }
        if self.formant_free.len() > crate::formants::MAX_FORMANTS - crate::formants::PARAM_BANDS
            || self.formant_free.iter().any(|b| !b.is_valid())
        {
            return Err(PresetError::ExtraInvalid(
                "formant_free: at most 3 bands, finite, bw > 0".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A user preset written before the formant editor existed has no
    /// count and no free bands — it must load as the 3-band patch it was.
    #[test]
    fn pre_editor_preset_loads_as_three_bands() {
        let json = format!(
            r#"{{"harmonics":{h:?},"formant_bw":[200.0,300.0,400.0],"formant_gain":[1.0,0.9,0.75]}}"#,
            h = vec![0.5_f32; N_HARMONICS]
        );
        let x: KubyzExtra = serde_json::from_str(&json).expect("old extra parses");
        assert_eq!(x.formant_count, 3);
        assert!(x.formant_free.is_empty());
        x.validate().expect("valid");
    }
}

pub type KubyzUserPreset = superduper_synth_core::user_preset::UserPreset<KubyzExtra>;
pub type KubyzRepo = superduper_synth_core::user_preset::PresetRepo<KubyzExtra>;

pub fn repo() -> &'static KubyzRepo {
    use std::sync::OnceLock;
    static REPO: OnceLock<KubyzRepo> = OnceLock::new();
    REPO.get_or_init(|| KubyzRepo::for_plugin("kubyz"))
}
