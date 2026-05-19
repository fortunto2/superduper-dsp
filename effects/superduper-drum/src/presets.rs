//! Factory presets for SuperDuper Drum. Quick starting points
//! covering the most common drum-machine flavours, plus a couple
//! of band-named picks to stay consistent with the rest of the suite.

use crate::PARAMS;

superduper_dsp_sdk::define_preset!(PARAMS);

// Param indices for terseness in the override tables.
use crate::voice_param_idx as v;
use crate::{P_DRIVE, P_MASTER};

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Default", &[]),

    // Trap — long sub kick, snappy snare, tight hats.
    Preset::from_overrides("Trap", &[
        (v(0, 0), -3.0), (v(0, 1), 0.9),  (v(0, 2), 1.0),
        (v(1, 1), 0.12), (v(1, 2), 0.65),
        (v(2, 1), 0.04),
        (v(3, 1), 0.6),
        (P_DRIVE, 0.15),
    ]),

    // Boom-bap — wide-open kick, fat snare, swung hats.
    Preset::from_overrides("Boom Bap", &[
        (v(0, 1), 0.55), (v(0, 0), -2.0),
        (v(1, 1), 0.22), (v(1, 2), 0.85),
        (v(2, 1), 0.06), (v(2, 2), 0.5),
        (P_DRIVE, 0.3), (P_MASTER, -2.0),
    ]),

    // 808 Hip-Hop — long sub kick, clap-heavy, drive on bus.
    Preset::from_overrides("808 Sub", &[
        (v(0, 0), -7.0), (v(0, 1), 1.3),  (v(0, 2), 1.0),
        (v(1, 2), 0.6),
        (v(4, 2), 0.85),
        (P_DRIVE, 0.4),
    ]),

    // Techno — tight kick, fast hats, no clap.
    Preset::from_overrides("Techno", &[
        (v(0, 1), 0.32), (v(0, 2), 1.0),
        (v(1, 1), 0.14),
        (v(2, 1), 0.05), (v(2, 2), 0.7),
        (v(3, 1), 0.18),
        (v(4, 2), 0.0),
        (P_DRIVE, 0.25),
    ]),

    // Joy Division (Closer kit) — gated dark, deep kick, snappy snare.
    Preset::from_overrides("Joy Division", &[
        (v(0, 0), -2.0), (v(0, 1), 0.45),
        (v(1, 0), -1.0), (v(1, 1), 0.18), (v(1, 2), 0.9),
        (v(2, 1), 0.05),
        (v(3, 1), 0.3),
        (P_DRIVE, 0.2), (P_MASTER, -2.0),
    ]),

    // Boards of Canada — slow lo-fi shuffle, broken drive.
    Preset::from_overrides("Boards of Canada", &[
        (v(0, 1), 0.7), (v(0, 0), -3.0),
        (v(1, 1), 0.3), (v(1, 2), 0.55),
        (v(2, 1), 0.12),
        (v(3, 1), 0.6),
        (P_DRIVE, 0.55), (P_MASTER, -4.0),
    ]),
];

pub fn apply(shared: &crate::SharedParamsInner, preset: &Preset) {
    use std::sync::atomic::Ordering;
    for (i, val) in preset.values.iter().enumerate() {
        if let Some(atom) = shared.params.get(i) {
            atom.store(*val, Ordering::Relaxed);
            shared.dirty_params[i].store(true, Ordering::Relaxed);
        }
    }
}
