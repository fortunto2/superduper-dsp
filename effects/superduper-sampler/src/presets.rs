//! Factory presets for SuperDuper Sampler. A preset here is a PLAYBACK
//! character — envelope, loop, filter, velocity response — never a sample:
//! the `Sample` index depends on whatever bank the user has scanned, so
//! `apply` skips it and whatever is loaded keeps playing, reshaped.

use crate::PARAMS;

superduper_dsp_sdk::define_preset!(PARAMS);

use crate::{
    P_ATTACK, P_CUTOFF, P_DECAY, P_FILTER_TYPE, P_LOOP, P_OUTPUT, P_RELEASE,
    P_RESO, P_REVERSE, P_SUSTAIN, P_VEL_AMP, P_VEL_CUTOFF,
};

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Default", &[]),
    // Drum machine hit: instant attack, full body, quick tail.
    Preset::from_overrides("One-Shot", &[
        (P_ATTACK, 0.001), (P_DECAY, 0.4), (P_SUSTAIN, 1.0), (P_RELEASE, 0.12),
        (P_VEL_AMP, 1.0),
    ]),
    // Long 808-style sub: ring past the note, LP keeps only the low end,
    // velocity opens the filter for accents.
    Preset::from_overrides("808 Long", &[
        (P_RELEASE, 1.4), (P_FILTER_TYPE, 1.0), (P_CUTOFF, 105.0),
        (P_VEL_CUTOFF, 12.0), (P_OUTPUT, -2.0),
    ]),
    // Gated chop for sliced breaks: the note length IS the sound.
    Preset::from_overrides("Tight Chop", &[
        (P_ATTACK, 0.001), (P_DECAY, 0.18), (P_SUSTAIN, 0.0), (P_RELEASE, 0.06),
    ]),
    // Sustained looped pad out of any material.
    Preset::from_overrides("Looped Pad", &[
        (P_LOOP, 1.0), (P_ATTACK, 0.6), (P_SUSTAIN, 0.85), (P_RELEASE, 1.5),
        (P_FILTER_TYPE, 1.0), (P_CUTOFF, 88.0),
    ]),
    // Reversed swell into the beat.
    Preset::from_overrides("Reverse Swell", &[
        (P_REVERSE, 1.0), (P_ATTACK, 0.4), (P_RELEASE, 0.9),
    ]),
    // HP-filtered stab — old-sampler vinyl chop with a resonant edge.
    Preset::from_overrides("Vinyl Stab", &[
        (P_FILTER_TYPE, 2.0), (P_CUTOFF, 62.0), (P_RESO, 0.25),
        (P_DECAY, 0.5), (P_SUSTAIN, 0.3), (P_RELEASE, 0.2),
    ]),
    // Dark held bass: loop on, LP closed down, even velocity.
    Preset::from_overrides("Sub Bass", &[
        (P_LOOP, 1.0), (P_FILTER_TYPE, 1.0), (P_CUTOFF, 70.0),
        (P_SUSTAIN, 1.0), (P_RELEASE, 0.3), (P_VEL_AMP, 0.6),
    ]),
];

/// Hand-maintained literal (NOT `PRESETS.len()`) so the const `PARAMS` table
/// can use it in the Preset param's `max` without a `PARAMS` ⇄ `PRESETS`
/// const-evaluation cycle. The assert keeps it honest.
pub const PRESET_COUNT: usize = 8;
const _: () = assert!(
    PRESET_COUNT == PRESETS.len(),
    "PRESET_COUNT out of sync with PRESETS — update PRESET_COUNT in presets.rs",
);
