//! Factory presets for SuperDuper Saturator.

use crate::{P_DRIVE, P_MIX, P_OUTPUT, P_TONE, P_TYPE, PARAMS};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Default", &[]),

    // Vocal warm: gentle tape drive, light high-shelf for air.
    Preset::from_overrides("Vocal Warm", &[
        (P_DRIVE, 6.0),
        (P_TYPE, 0.0), // Tape
        (P_TONE, 0.2),
        (P_OUTPUT, -1.0),
        (P_MIX, 0.7),
    ]),

    // Bass thick: tube character, slight darkening.
    Preset::from_overrides("Bass Thick", &[
        (P_DRIVE, 9.0),
        (P_TYPE, 1.0), // Tube
        (P_TONE, -0.2),
        (P_OUTPUT, -2.0),
        (P_MIX, 0.85),
    ]),

    // Drum crush: hard tanh + bright top — parallel use recommended (lower Mix).
    Preset::from_overrides("Drum Crush", &[
        (P_DRIVE, 18.0),
        (P_TYPE, 2.0), // Soft (tanh)
        (P_TONE, 0.3),
        (P_OUTPUT, -6.0),
        (P_MIX, 0.4),
    ]),

    // Guitar grit: lots of drive, tube character, slight darkness.
    Preset::from_overrides("Guitar Grit", &[
        (P_DRIVE, 14.0),
        (P_TYPE, 1.0),
        (P_TONE, -0.1),
        (P_OUTPUT, -3.0),
        (P_MIX, 1.0),
    ]),

    // Master glue: subtle tape, full wet, ~unity output.
    Preset::from_overrides("Master Glue", &[
        (P_DRIVE, 3.0),
        (P_TYPE, 0.0),
        (P_TONE, 0.0),
        (P_OUTPUT, 0.0),
        (P_MIX, 1.0),
    ]),
];

pub fn apply(shared: &crate::SharedParamsInner, preset: &Preset) {
    use std::sync::atomic::Ordering;
    for (i, v) in preset.values.iter().enumerate() {
        if let Some(atom) = shared.params.get(i) {
            atom.store(*v, Ordering::Relaxed);
        }
    }
}
