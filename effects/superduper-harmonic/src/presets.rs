//! Factory presets for SuperDuper Harmonic Clean.

use crate::{PARAMS, P_AMOUNT, P_BANDWIDTH, P_MIX, P_OUTPUT, P_RANGE, P_TRANSIENT};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Init", &[]),

    // Default working point for a bass jaw-harp drone: solid noise cut, medium
    // keep-band, plucks well preserved, full wet.
    Preset::from_overrides("Kubyz Clean", &[
        (P_AMOUNT, 0.7),
        (P_BANDWIDTH, 0.5),
        (P_TRANSIENT, 0.6),
        (P_MIX, 1.0),
        (P_OUTPUT, 0.0),
        (P_RANGE, 60.0),
    ]),

    // Transparent — light touch, wide keep-band, almost no risk of dulling the
    // tone. Use when the pickup is already fairly clean and you just want to
    // shave the hiss without changing the character.
    Preset::from_overrides("Gentle / Transparent", &[
        (P_AMOUNT, 0.4),
        (P_BANDWIDTH, 0.8),
        (P_TRANSIENT, 0.7),
        (P_MIX, 1.0),
        (P_OUTPUT, 0.0),
        (P_RANGE, 60.0),
    ]),

    // Aggressive — maximum noise squeeze for a nasty, buzzy contact pickup.
    // Narrow keep-band + high Amount; needs a steady, well-tracked drone, and
    // Range nudged up so it doesn't chase an octave-down ghost.
    Preset::from_overrides("Aggressive", &[
        (P_AMOUNT, 0.92),
        (P_BANDWIDTH, 0.12),
        (P_TRANSIENT, 0.5),
        (P_MIX, 1.0),
        (P_OUTPUT, 1.0),
        (P_RANGE, 70.0),
    ]),

    // Transient Max — for a percussive, pluck-forward style. Comb fully
    // re-opens on every attack so the plucks stay razor-sharp; the sustained
    // tail still gets cleaned.
    Preset::from_overrides("Transient Max", &[
        (P_AMOUNT, 0.75),
        (P_BANDWIDTH, 0.4),
        (P_TRANSIENT, 1.0),
        (P_MIX, 1.0),
        (P_OUTPUT, 0.0),
        (P_RANGE, 60.0),
    ]),
];

/// Apply a preset — store each value into the matching atomic and mark dirty so
/// the host's FX automation lane records the change (lesson 21d/24).
pub fn apply(shared: &crate::SharedParamsInner, preset: &Preset) {
    use std::sync::atomic::Ordering;
    for (i, v) in preset.values.iter().enumerate() {
        if let Some(atom) = shared.params.get(i) {
            atom.store(*v, Ordering::Relaxed);
        }
        if let Some(d) = shared.dirty_params.get(i) {
            d.store(true, Ordering::Relaxed);
        }
    }
}
