//! Built-in presets for SuperDuper Reverb.
//!
//! Each preset is just a `[f32; PARAMS.len()]` keyed by the `P_*` index
//! constants from `lib.rs`. Applying a preset writes those values straight
//! into the shared atomics — the audio thread picks them up on the next
//! sample, the host's slider position updates on its next get_value query.

use crate::{P_DAMP, P_DECAY, P_DUCK_AMOUNT, P_DUCK_ATTACK, P_DUCK_RELEASE, P_MIX, P_MOD,
            P_PREDELAY, P_SIZE, P_WIDTH, PARAMS};

pub struct Preset {
    pub name: &'static str,
    pub values: [f32; PARAMS.len()],
}

impl Preset {
    /// Build a preset from a slice of (param_index, value) pairs. Unspecified
    /// params fall back to the table default — way less typo-prone than
    /// hand-writing a full [f32; 10] array per preset.
    const fn from_overrides(name: &'static str, overrides: &[(usize, f32)]) -> Self {
        let mut values = [0.0_f32; PARAMS.len()];
        let mut i = 0;
        while i < PARAMS.len() {
            values[i] = PARAMS[i].default as f32;
            i += 1;
        }
        i = 0;
        while i < overrides.len() {
            values[overrides[i].0] = overrides[i].1;
            i += 1;
        }
        Self { name, values }
    }
}

/// Catalog of factory presets shown in the GUI dropdown.
pub static PRESETS: &[Preset] = &[
    // Defaults straight out of the PARAMS table — useful as a reset.
    Preset::from_overrides("Default", &[]),

    // Vocal plate: bright, fast, low mix — sits behind the dry vocal without
    // pushing it back in the mix. Short pre-delay keeps consonants tight.
    Preset::from_overrides("Vocal Plate", &[
        (P_SIZE, 0.6),
        (P_DECAY, 0.65),
        (P_DAMP, 0.3),
        (P_PREDELAY, 25.0),
        (P_MOD, 0.3),
        (P_WIDTH, 1.0),
        (P_MIX, 0.22),
    ]),

    // Drum room: small, neutral damping, low pre-delay. Glue without wash.
    Preset::from_overrides("Drum Room", &[
        (P_SIZE, 0.5),
        (P_DECAY, 0.5),
        (P_DAMP, 0.45),
        (P_PREDELAY, 8.0),
        (P_MOD, 0.1),
        (P_WIDTH, 0.85),
        (P_MIX, 0.18),
    ]),

    // Ambient hall: big, lush, deep modulation. Long pre-delay keeps the
    // source clear before the wash kicks in.
    Preset::from_overrides("Ambient Hall", &[
        (P_SIZE, 1.3),
        (P_DECAY, 0.9),
        (P_DAMP, 0.4),
        (P_PREDELAY, 45.0),
        (P_MOD, 0.65),
        (P_WIDTH, 1.0),
        (P_MIX, 0.5),
    ]),

    // Bass plate: short and DARK so the reverb doesn't muddy the low end.
    // Heavy damping rolls off above ~3 kHz, mix kept conservative.
    Preset::from_overrides("Bass Plate", &[
        (P_SIZE, 0.7),
        (P_DECAY, 0.6),
        (P_DAMP, 0.8),
        (P_PREDELAY, 10.0),
        (P_MOD, 0.2),
        (P_WIDTH, 0.7),
        (P_MIX, 0.18),
    ]),

    // Cathedral: maximum size, near-max decay, gentle damping, long pre-delay.
    // Heavy on the wet, so meant for sends, not insert.
    Preset::from_overrides("Cathedral", &[
        (P_SIZE, 1.5),
        (P_DECAY, 0.92),
        (P_DAMP, 0.55),
        (P_PREDELAY, 80.0),
        (P_MOD, 0.5),
        (P_WIDTH, 1.0),
        (P_MIX, 0.45),
    ]),

    // Slap echo: very small size, low decay, big pre-delay — the classic
    // rockabilly "one bounce" sound.
    Preset::from_overrides("Slap Echo", &[
        (P_SIZE, 0.3),
        (P_DECAY, 0.4),
        (P_DAMP, 0.4),
        (P_PREDELAY, 110.0),
        (P_MOD, 0.05),
        (P_WIDTH, 0.9),
        (P_MIX, 0.35),
    ]),

    // EDM ducked: medium-long tail with auto-ducking dialled in — kick
    // through dry input keeps the reverb out of the way of the beat.
    Preset::from_overrides("EDM Ducked", &[
        (P_SIZE, 1.0),
        (P_DECAY, 0.78),
        (P_DAMP, 0.35),
        (P_PREDELAY, 20.0),
        (P_MOD, 0.4),
        (P_WIDTH, 1.0),
        (P_MIX, 0.4),
        (P_DUCK_AMOUNT, 8.0),
        (P_DUCK_ATTACK, 5.0),
        (P_DUCK_RELEASE, 220.0),
    ]),
];

/// Apply a preset by writing every value into the shared atomics.
pub fn apply(shared: &crate::SharedParamsInner, preset: &Preset) {
    use std::sync::atomic::Ordering;
    for (i, v) in preset.values.iter().enumerate() {
        if let Some(atom) = shared.params.get(i) {
            atom.store(*v, Ordering::Relaxed);
        }
    }
}
