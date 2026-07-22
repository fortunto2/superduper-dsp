//! Shared DSP code carried over from the original `rust-synth` project.
//!
//! This crate is the home for voice builders, the Supermassive-style reverb,
//! and the math utilities (harmony / rhythm / motif / etc.) that every
//! SuperDuper synth plugin will eventually share.
//!
//! For now we expose the bare minimum needed to ship the standalone
//! Supermass reverb effect. Voices and math will follow as the Ambient and
//! Pad plugins land.

pub mod analysis;
pub mod dsp_blocks;
pub mod formant;
pub mod linphase;
pub mod loudness;
pub mod nam;
pub mod pitch;
pub mod psola; // TD-PSOLA pitch/formant shifter (extracted from superduper-pitch; shared with superduper-tune + iOS)
pub mod spectral;
pub mod supermass;
pub mod user_preset;
pub mod wav;
pub mod wave_osc; // wavetable oscillator/voice (extracted from superduper-wave so it reaches iOS too)
pub mod drum_voices; // 6 analog drum voices (extracted from superduper-drum)
pub mod kubyz; // Bashkir jaw-harp additive model (extracted from superduper-kubyz)

#[cfg(feature = "gui")]
pub mod gui;
