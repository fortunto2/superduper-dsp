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
pub mod supermass;
pub mod user_preset;
pub mod wav;

#[cfg(feature = "gui")]
pub mod gui;
