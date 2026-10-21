//! Factory presets for SuperDuper Double.

use crate::{
    PARAMS, P_DRY, P_KEY_OPEN, P_LEVEL, P_MODE, P_PITCH_VAR, P_RETUNE, P_SPREAD, P_TIMING,
    P_VOICES, P_FORMANT,
};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    // The default: two doubles, wide, a tight-but-human performance.
    Preset::from_overrides("Double Track", &[]),
    // One double, close and centred-ish — thickening, not width.
    Preset::from_overrides("Tight Single", &[
        (P_VOICES, 1.0),
        (P_TIMING, 18.0),
        (P_PITCH_VAR, 5.0),
        (P_SPREAD, 0.0),
        (P_LEVEL, -6.0),
    ]),
    // Four loose doubles, a gang vocal.
    Preset::from_overrides("Gang Vocal", &[
        (P_VOICES, 4.0),
        (P_TIMING, 55.0),
        (P_PITCH_VAR, 16.0),
        (P_SPREAD, 1.0),
        (P_LEVEL, -4.0),
    ]),
    // A slightly darker, bigger second singer.
    Preset::from_overrides("Other Singer", &[
        (P_VOICES, 1.0),
        (P_TIMING, 35.0),
        (P_PITCH_VAR, 10.0),
        (P_FORMANT, -1.5),
        (P_SPREAD, 0.6),
    ]),
    // Auto-Tuned doubles under a natural lead (modern pop stack).
    Preset::from_overrides("Tuned Stack", &[
        (P_MODE, 1.0),
        (P_VOICES, 2.0),
        (P_RETUNE, 10.0),
        (P_TIMING, 25.0),
    ]),
    // The lead gets snapped, the doubles stay human.
    Preset::from_overrides("Tuned Lead", &[
        (P_MODE, 2.0),
        (P_RETUNE, 40.0),
    ]),
    // Doubles only, to print onto their own track.
    Preset::from_overrides("Doubles Only", &[
        (P_DRY, 0.0),
        (P_LEVEL, 0.0),
    ]),
    // Doubles open up only when the key track (master, drums) gets loud.
    // Until a key is picked they play in full (no key = no reaction).
    Preset::from_overrides("Chorus Swell", &[
        (P_VOICES, 3.0),
        (P_KEY_OPEN, 1.0),
    ]),
];

pub fn apply(shared: &crate::SharedParamsInner, preset: &Preset) {
    use std::sync::atomic::Ordering;
    for (i, v) in preset.values.iter().enumerate() {
        if let Some(atom) = shared.params.get(i) {
            atom.store(*v, Ordering::Relaxed);
            if let Some(d) = shared.dirty_params.get(i) {
                d.store(true, Ordering::Relaxed);
            }
        }
    }
}
