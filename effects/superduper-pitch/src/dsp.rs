//! SuperDuper Pitch — TD-PSOLA pitch/formant shifter.
//!
//! The engine itself now lives in [`superduper_synth_core::psola`] so it can be
//! shared with **SuperDuper Tune** (autotune) and cross-compiled to iOS
//! (live2play), the same way `wave_osc` / `kubyz` were extracted. This module
//! re-exports it under the original `crate::dsp` path so the plugin, its GUI,
//! `pvoc.rs`, and every `tests/` harness keep compiling unchanged. The full
//! algorithm documentation lives on the engine source.

pub use superduper_synth_core::psola::{PitchParams, PitchShifter};
