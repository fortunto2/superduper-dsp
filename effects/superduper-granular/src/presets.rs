//! Factory presets for SuperDuper Granular.
//!
//! `PRESETS.len()` must equal [`crate::PRESET_COUNT`] (which the Preset param's
//! max is built from); the `const _` below enforces it at compile time.

use crate::{
    P_DENSITY, P_DIV, P_FEEDBACK, P_FREEZE, P_JITTER, P_MIX, P_OUTPUT, P_PITCH, P_POSITION,
    P_REVERSE, P_SHAPE, P_SIZE, P_SPRAY, P_SPREAD, P_SYNC, PARAMS,
};
use superduper_synth_core::granular::{SHAPE_PERC, SHAPE_TUKEY};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Default", &[]),

    // The headline move: hit Freeze (or the sustain pedal) on a held note and
    // the cloud keeps grinding that moment forever. Wide, dense, long grains.
    Preset::from_overrides("Freeze Pad", &[
        (P_FREEZE, 1.0),
        (P_DENSITY, 45.0),
        (P_SIZE, 240.0),
        (P_SPRAY, 0.55),
        (P_SPREAD, 0.85),
        (P_MIX, 1.0),
    ]),

    // Live cloud over a voice — dense enough to smear, short enough to keep
    // the words' rhythm.
    Preset::from_overrides("Voice Cloud", &[
        (P_DENSITY, 32.0),
        (P_SIZE, 120.0),
        (P_SPRAY, 0.3),
        (P_SPREAD, 0.6),
        (P_MIX, 0.7),
    ]),

    // Octave-up sparkle layered under the dry signal.
    Preset::from_overrides("Shimmer +12", &[
        (P_PITCH, 12.0),
        (P_DENSITY, 50.0),
        (P_SIZE, 90.0),
        (P_SPRAY, 0.35),
        (P_SPREAD, 0.9),
        (P_MIX, 0.45),
    ]),

    // Octave-down drone — long grains, slow, sits under everything.
    Preset::from_overrides("Sub Drone −12", &[
        (P_PITCH, -12.0),
        (P_DENSITY, 22.0),
        (P_SIZE, 320.0),
        (P_SPRAY, 0.4),
        (P_MIX, 0.6),
    ]),

    // Pointillist: percussive windows, sparse, wide, pitch-scattered.
    Preset::from_overrides("Pointillist", &[
        (P_SHAPE, SHAPE_PERC as f32),
        (P_DENSITY, 14.0),
        (P_SIZE, 45.0),
        (P_JITTER, 5.0),
        (P_SPRAY, 0.5),
        (P_SPREAD, 1.0),
    ]),

    // Rhythmic stutter locked to the grid — grains fire on 1/16 notes, read from
    // just behind the write head so it reads as a beat-repeat, not a wash.
    Preset::from_overrides("Grid Stutter", &[
        (P_SYNC, 1.0),
        (P_DIV, 10.0), // 1/16
        (P_SIZE, 70.0),
        (P_SPRAY, 0.02),
        (P_POSITION, 0.02),
        (P_SHAPE, SHAPE_TUKEY as f32),
        (P_SPREAD, 0.25),
    ]),

    // Everything backwards — the classic reverse-verb-ish wash.
    Preset::from_overrides("Reverse Wash", &[
        (P_REVERSE, 1.0),
        (P_DENSITY, 30.0),
        (P_SIZE, 260.0),
        (P_SPRAY, 0.5),
        (P_SPREAD, 0.7),
        (P_MIX, 0.65),
    ]),

    // Dense + feedback: grains granulating grains until the source dissolves.
    Preset::from_overrides("Smear", &[
        (P_DENSITY, 110.0),
        (P_SIZE, 200.0),
        (P_SPRAY, 0.75),
        (P_FEEDBACK, 0.55),
        (P_SPREAD, 0.8),
    ]),

    // Maximum self-feeding texture — slow bloom out of whatever you play.
    Preset::from_overrides("Texture Bloom", &[
        (P_DENSITY, 65.0),
        (P_SIZE, 160.0),
        (P_SPRAY, 0.65),
        (P_JITTER, 3.0),
        (P_FEEDBACK, 0.8),
        (P_SPREAD, 1.0),
        (P_OUTPUT, -3.0),
    ]),
];

/// A drifted count would make the host address presets that don't exist and
/// leave the last one unreachable. Same guard the other plugins use — declaring
/// `PRESET_COUNT` separately is only necessary because referencing `PRESETS`
/// from inside `PARAMS` is a const-eval cycle (E0391).
const _: () = assert!(
    crate::PRESET_COUNT == PRESETS.len(),
    "PRESET_COUNT out of sync with PRESETS"
);
