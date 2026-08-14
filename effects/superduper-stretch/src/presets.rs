//! Factory presets for SuperDuper Stretch.
//!
//! `PRESETS.len()` must equal [`crate::PRESET_COUNT`] (which the Preset param's
//! max is built from); the `const _` below enforces it at compile time.

use crate::{
    P_FREEZE, P_LENGTH, P_MIX, P_OUTPUT, P_PITCH, P_SMOOTH, P_STRETCH, P_TONAL, P_WINDOW, PARAMS,
};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Default", &[]),

    // What people mean by "paulstretched": huge ratio, long window, fully random
    // phase. Anything recognisable becomes weather.
    Preset::from_overrides("Paulstretch Classic", &[
        (P_STRETCH, 20.0),
        (P_WINDOW, 3.0), // 683 ms
        (P_TONAL, 0.0),
        (P_MIX, 1.0),
    ]),

    // Sing one note → hold it forever. Freeze on, short region so the loop stays
    // on the vowel rather than wandering through the whole take.
    Preset::from_overrides("Freeze Pad", &[
        (P_FREEZE, 1.0),
        (P_STRETCH, 16.0),
        (P_WINDOW, 3.0),
        (P_LENGTH, 2.5),
        (P_MIX, 1.0),
    ]),

    // The bridge that makes a sung note into a bed the kubyz can sit on: enough
    // Tonal to keep the pitch legible, enough Smooth to lose the consonants.
    // Chain SuperDuper Formant after this and the pad speaks.
    Preset::from_overrides("Voice → Pad", &[
        (P_STRETCH, 12.0),
        (P_WINDOW, 2.0),
        (P_TONAL, 0.2),
        (P_SMOOTH, 0.25),
        (P_MIX, 1.0),
    ]),

    // Mostly phase-preserving: a plain slow-motion rather than a smear. Useful
    // on drums / speech where you want to still hear the event.
    Preset::from_overrides("Slow Motion", &[
        (P_STRETCH, 4.0),
        (P_WINDOW, 1.0), // 171 ms
        (P_TONAL, 0.85),
        (P_MIX, 1.0),
    ]),

    // Maximum ratio, maximum window, heavy smoothing — barely moves.
    Preset::from_overrides("Glacier", &[
        (P_STRETCH, 50.0),
        (P_WINDOW, 4.0), // 1.37 s
        (P_SMOOTH, 0.45),
        (P_OUTPUT, -2.0),
    ]),

    // Shimmer wash an octave up, sits over the dry source.
    Preset::from_overrides("Octave Wash", &[
        (P_STRETCH, 16.0),
        (P_WINDOW, 3.0),
        (P_PITCH, 12.0),
        (P_MIX, 0.5),
    ]),

    // Sub bed an octave down — long window so it's pure weight, no articulation.
    Preset::from_overrides("Sub Bed", &[
        (P_STRETCH, 16.0),
        (P_WINDOW, 3.0),
        (P_PITCH, -12.0),
        (P_SMOOTH, 0.3),
        (P_MIX, 0.6),
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
