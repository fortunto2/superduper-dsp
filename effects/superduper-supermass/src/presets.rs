//! Built-in presets for SuperDuper Supermass.
//!
//! The cascade reverb has a 28-second second-stage T60, so most presets are
//! conservative on mix — anything past ~0.5 turns whatever you feed it into
//! a giant wash. Drive + Tilt are the main tone-shapers; ducking keeps the
//! wash out of the way of rhythmic content.

use crate::{P_DRIVE, P_DUCK_AMOUNT, P_DUCK_ATTACK, P_DUCK_RELEASE, P_MIX, P_TILT, P_WIDTH, PARAMS};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Default", &[]),

    // Cinematic pad: full width, bright tilt, big mix. Letterboxed-movie wash.
    Preset::from_overrides("Cinematic Pad", &[
        (P_MIX, 0.55),
        (P_WIDTH, 1.0),
        (P_DRIVE, 0.1),
        (P_TILT, 0.25),
    ]),

    // Dark cave: heavy tilt down, slight drive — sounds like a tunnel.
    Preset::from_overrides("Dark Cave", &[
        (P_MIX, 0.5),
        (P_WIDTH, 0.85),
        (P_DRIVE, 0.25),
        (P_TILT, -0.5),
    ]),

    // Vocal wash: light mix, neutral tilt, narrow width so the dry stays
    // centered.
    Preset::from_overrides("Vocal Wash", &[
        (P_MIX, 0.32),
        (P_WIDTH, 0.7),
        (P_DRIVE, 0.0),
        (P_TILT, 0.1),
    ]),

    // Synth pad shimmer: bright tilt, full width, moderate mix.
    Preset::from_overrides("Synth Shimmer", &[
        (P_MIX, 0.45),
        (P_WIDTH, 1.0),
        (P_DRIVE, 0.15),
        (P_TILT, 0.6),
    ]),

    // EDM ducked: heavy ducking with kick → side-chain. Lets reverb live
    // between every bass hit without smearing the groove.
    Preset::from_overrides("EDM Ducked", &[
        (P_MIX, 0.5),
        (P_WIDTH, 1.0),
        (P_DRIVE, 0.05),
        (P_TILT, 0.2),
        (P_DUCK_AMOUNT, 10.0),
        (P_DUCK_ATTACK, 5.0),
        (P_DUCK_RELEASE, 250.0),
    ]),

    // Drone bed: maximum mix, slightly dark tilt, no drive. Ambient layers.
    Preset::from_overrides("Drone Bed", &[
        (P_MIX, 0.7),
        (P_WIDTH, 1.0),
        (P_DRIVE, 0.0),
        (P_TILT, -0.2),
    ]),

    // Vocal send ducked: aux send + vocal into sidechain. Big cinematic
    // wash that gets out of the way the moment the vocal speaks.
    Preset::from_overrides("Vocal Send Ducked", &[
        (P_MIX, 1.0),
        (P_WIDTH, 1.0),
        (P_DRIVE, 0.05),
        (P_TILT, 0.1),
        (P_DUCK_AMOUNT, 14.0),
        (P_DUCK_ATTACK, 5.0),
        (P_DUCK_RELEASE, 300.0),
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
