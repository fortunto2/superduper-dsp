//! Factory presets for SuperDuper Formant.
//!
//! The vowel presets come from Peterson-Barney (1952) male averages, the same
//! table `synth_core::formant::FORMANT_PRESETS` carries; Bashkir Kubyz is
//! measured off the KubizBeat reference material.
//!
//! `PRESETS.len()` must equal [`crate::PRESET_COUNT`] (which the Preset param's
//! max is built from); the `const _` below enforces it at compile time.

use crate::dsp::{MODE_FOLLOW, MODE_MOTION};
use crate::{
    P_DEPTH, P_DIV, P_DRIVE, P_F1, P_F2, P_F3, P_FOLLOW, P_GLIDE, P_MIX, P_MODE, P_OUTPUT, P_PATH,
    P_RATE, P_STEREO, P_SYNC, P_WIDTH, PARAMS,
};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Default", &[]),

    // ---- Vowels: park the pad on one mouth shape ------------------------
    Preset::from_overrides("Vowel A (open)", &[
        (P_F1, 730.0), (P_F2, 1090.0), (P_F3, 2440.0),
    ]),
    Preset::from_overrides("Vowel E", &[
        (P_F1, 530.0), (P_F2, 1840.0), (P_F3, 2480.0),
    ]),
    Preset::from_overrides("Vowel I (closed)", &[
        (P_F1, 270.0), (P_F2, 2290.0), (P_F3, 3010.0),
    ]),
    Preset::from_overrides("Vowel O", &[
        (P_F1, 570.0), (P_F2, 840.0), (P_F3, 2410.0),
    ]),
    Preset::from_overrides("Vowel U (dark)", &[
        (P_F1, 300.0), (P_F2, 870.0), (P_F3, 2240.0),
    ]),
    Preset::from_overrides("Bashkir Kubyz", &[
        (P_F1, 705.0), (P_F2, 1301.0), (P_F3, 2165.0),
        (P_WIDTH, 1.1),
    ]),

    // ---- The flagship: a sung phrase hands over to the instrument -------
    // Voice into the 'Voice' sidechain, kubyz (or any drone) on the insert.
    // Glide is short so consonant-to-vowel moves read as articulation, and
    // the tracker's gate freezes the last vowel when the singing stops — that
    // freeze is what makes the hand-off sound continuous rather than cut.
    Preset::from_overrides("Voice → Kubyz", &[
        (P_MODE, MODE_FOLLOW as f32),
        (P_FOLLOW, 1.0),
        (P_GLIDE, 22.0),
        (P_WIDTH, 0.9),
        (P_MIX, 0.9),
    ]),
    // Same idea, gentler: keeps more of the untouched drone underneath so the
    // articulation colours the sound instead of replacing it.
    Preset::from_overrides("Voice Colour", &[
        (P_MODE, MODE_FOLLOW as f32),
        (P_FOLLOW, 0.75),
        (P_GLIDE, 60.0),
        (P_WIDTH, 1.4),
        (P_MIX, 0.5),
    ]),

    // ---- Motion: articulation with nobody singing ----------------------
    Preset::from_overrides("Talking Drone", &[
        (P_MODE, MODE_MOTION as f32),
        (P_PATH, 2.0), // Figure-8
        (P_RATE, 1.2),
        (P_DEPTH, 0.7),
        (P_WIDTH, 0.85),
    ]),
    // Rhythmic wah locked to the host grid — 1/8 notes, straight line path.
    Preset::from_overrides("Wah in Time", &[
        (P_MODE, MODE_MOTION as f32),
        (P_PATH, 4.0), // Line
        (P_SYNC, 1.0),
        (P_DIV, 7.0), // 1/8
        (P_DEPTH, 0.85),
        (P_WIDTH, 0.6),
        (P_F2, 1400.0),
    ]),
    // Wide: L and R walk the trajectory in anti-phase.
    Preset::from_overrides("Wide Mouth", &[
        (P_MODE, MODE_MOTION as f32),
        (P_PATH, 0.0), // Circle
        (P_RATE, 0.35),
        (P_DEPTH, 0.6),
        (P_STEREO, 1.0),
    ]),
    // Narrow bands + drive = a growling, throaty resonance.
    Preset::from_overrides("Growl", &[
        (P_F1, 600.0), (P_F2, 1000.0), (P_F3, 2100.0),
        (P_WIDTH, 0.5),
        (P_DRIVE, 0.65),
        (P_OUTPUT, -2.0),
    ]),
];

/// Write a preset into the shared atomics. Marks every param dirty so the
/// host's automation lane sees the recall (lesson 21d).
/// A drifted count would make the host address presets that don't exist and
/// leave the last one unreachable. Same guard the other plugins use — declaring
/// `PRESET_COUNT` separately is only necessary because referencing `PRESETS`
/// from inside `PARAMS` is a const-eval cycle (E0391).
const _: () = assert!(
    crate::PRESET_COUNT == PRESETS.len(),
    "PRESET_COUNT out of sync with PRESETS"
);
