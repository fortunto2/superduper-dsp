use crate::{
    PARAMS, P_ATTACK, P_CUTOFF, P_DECAY, P_DRIVE, P_MODULATION, P_OUTPUT, P_RELEASE, P_RESONANCE,
    P_SUSTAIN, P_WIDTH,
};

superduper_dsp_sdk::define_preset!(PARAMS);

/// Preset count, decoupled from the `Preset` type. `PARAMS`' Preset-selector
/// param needs the preset count as its `max`, but reading `PRESETS.len()`
/// from inside `PARAMS` creates a const-eval cycle: `Preset.values` is
/// `[f32; PARAMS.len()]`, so the type of `PRESETS` depends on `PARAMS`, and
/// `PARAMS`' `max` would depend on `PRESETS`. This standalone count is built
/// from a names array that does NOT touch the `Preset` type, breaking the
/// cycle. A static assert below keeps it equal to `PRESETS.len()`.
pub const PRESET_COUNT: usize = PRESET_NAMES.len();

const PRESET_NAMES: &[&str] = &[
    "Init",
    "Pad Default",
    "Slow Strings",
    "Choir",
    "Glass",
    "Wide Ambient",
    "Pluck",
    "Vangelis (Blade Runner)",
    "Joy Division (Atmosphere)",
    "Cocteau Twins (Pad)",
    "Boards of Canada (Lo-Fi)",
];

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Init", &[]),

    // Default — broad, warm, moderate attack and tail. The "always works" patch.
    Preset::from_overrides("Pad Default", &[
        (P_CUTOFF, 3500.0),
        (P_RESONANCE, 0.18),
        (P_MODULATION, 6.0),
        (P_DRIVE, 0.20),
        (P_WIDTH, 8.0),
        (P_ATTACK, 0.4),
        (P_DECAY, 0.6),
        (P_SUSTAIN, 0.8),
        (P_RELEASE, 1.5),
        (P_OUTPUT, -8.0),
    ]),

    // Slow swell strings — long attack, sustain stays full, decay irrelevant.
    Preset::from_overrides("Slow Strings", &[
        (P_CUTOFF, 5500.0),
        (P_RESONANCE, 0.10),
        (P_MODULATION, 12.0),
        (P_DRIVE, 0.12),
        (P_WIDTH, 14.0),
        (P_ATTACK, 1.4),
        (P_DECAY, 0.8),
        (P_SUSTAIN, 0.92),
        (P_RELEASE, 2.5),
        (P_OUTPUT, -10.0),
    ]),

    // Choir — bright with breath-like modulation, instant attack.
    Preset::from_overrides("Choir", &[
        (P_CUTOFF, 6500.0),
        (P_RESONANCE, 0.06),
        (P_MODULATION, 18.0),
        (P_DRIVE, 0.18),
        (P_WIDTH, 20.0),
        (P_ATTACK, 0.05),
        (P_DECAY, 0.4),
        (P_SUSTAIN, 0.85),
        (P_RELEASE, 1.8),
        (P_OUTPUT, -10.0),
    ]),

    // Glass bells — short, hard attack, long decay to sustain, plucky feel.
    Preset::from_overrides("Glass", &[
        (P_CUTOFF, 8500.0),
        (P_RESONANCE, 0.35),
        (P_MODULATION, 4.0),
        (P_DRIVE, 0.10),
        (P_WIDTH, 6.0),
        (P_ATTACK, 0.005),
        (P_DECAY, 1.2),
        (P_SUSTAIN, 0.35),
        (P_RELEASE, 2.4),
        (P_OUTPUT, -8.0),
    ]),

    // Wide ambient pad — huge width, lots of modulation, long release.
    Preset::from_overrides("Wide Ambient", &[
        (P_CUTOFF, 2500.0),
        (P_RESONANCE, 0.22),
        (P_MODULATION, 22.0),
        (P_DRIVE, 0.25),
        (P_WIDTH, 28.0),
        (P_ATTACK, 0.8),
        (P_DECAY, 1.0),
        (P_SUSTAIN, 0.78),
        (P_RELEASE, 4.0),
        (P_OUTPUT, -10.0),
    ]),

    // Pluck — fast attack, fast decay to no sustain, short release. Stab pad.
    Preset::from_overrides("Pluck", &[
        (P_CUTOFF, 4200.0),
        (P_RESONANCE, 0.32),
        (P_MODULATION, 2.0),
        (P_DRIVE, 0.30),
        (P_WIDTH, 4.0),
        (P_ATTACK, 0.003),
        (P_DECAY, 0.35),
        (P_SUSTAIN, 0.0),
        (P_RELEASE, 0.25),
        (P_OUTPUT, -6.0),
    ]),

    // ----- Band-flavoured presets -----
    // Vangelis CS-80 brass-pad: slow swelling brass, wide, deep mod,
    // long release. Pair with the Chorus "Vangelis" preset and the
    // Reverb "Vangelis (Blade Runner)" preset for the full Blade
    // Runner end-credits wash.
    Preset::from_overrides("Vangelis (Blade Runner)", &[
        (P_CUTOFF, 3200.0),
        (P_RESONANCE, 0.20),
        (P_MODULATION, 22.0),
        (P_DRIVE, 0.35),
        (P_WIDTH, 18.0),
        (P_ATTACK, 0.9),
        (P_DECAY, 1.2),
        (P_SUSTAIN, 0.95),
        (P_RELEASE, 3.5),
        (P_OUTPUT, -7.0),
    ]),

    // Joy Division "Atmosphere" — dark, slow attack synth pad. Through
    // the Chorus "Joy Division" + Reverb "Atmosphere" presets to taste.
    Preset::from_overrides("Joy Division (Atmosphere)", &[
        (P_CUTOFF, 1700.0),
        (P_RESONANCE, 0.28),
        (P_MODULATION, 5.0),
        (P_DRIVE, 0.42),
        (P_WIDTH, 10.0),
        (P_ATTACK, 0.6),
        (P_DECAY, 1.4),
        (P_SUSTAIN, 0.85),
        (P_RELEASE, 2.8),
        (P_OUTPUT, -8.0),
    ]),

    // Cocteau Twins guitar-pad. Bright, gauzy, ethereal — pair with
    // a heavy shimmer reverb (Cocteau Twins preset on Reverb).
    Preset::from_overrides("Cocteau Twins (Pad)", &[
        (P_CUTOFF, 7000.0),
        (P_RESONANCE, 0.12),
        (P_MODULATION, 14.0),
        (P_DRIVE, 0.18),
        (P_WIDTH, 22.0),
        (P_ATTACK, 0.25),
        (P_DECAY, 0.9),
        (P_SUSTAIN, 0.9),
        (P_RELEASE, 3.0),
        (P_OUTPUT, -9.0),
    ]),

    // Boards of Canada lo-fi analog pad — narrow band, wobble heavy
    // (modulation), short release. Lo-fi but warm.
    Preset::from_overrides("Boards of Canada (Lo-Fi)", &[
        (P_CUTOFF, 2200.0),
        (P_RESONANCE, 0.25),
        (P_MODULATION, 28.0),
        (P_DRIVE, 0.45),
        (P_WIDTH, 6.0),
        (P_ATTACK, 0.3),
        (P_DECAY, 0.7),
        (P_SUSTAIN, 0.75),
        (P_RELEASE, 1.4),
        (P_OUTPUT, -8.0),
    ]),
];

// Keep PRESET_NAMES (used for the cycle-free count) in lock-step with the
// real PRESETS table — counts must match or the Preset-selector param's max
// would be wrong. A length mismatch fails the build here.
const _: () = assert!(PRESET_COUNT == PRESETS.len());

pub fn apply(shared: &crate::SharedParamsInner, preset: &Preset) {
    use std::sync::atomic::Ordering;
    for (i, v) in preset.values.iter().enumerate() {
        if let Some(atom) = shared.params.get(i) {
            atom.store(*v, Ordering::Relaxed);
        }
    }
}
