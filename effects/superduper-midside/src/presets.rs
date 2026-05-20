use crate::{PARAMS, P_MID, P_MODE, P_OUTPUT, P_SIDE, P_WIDTH};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Init", &[]),

    Preset::from_overrides("Mono fold", &[
        (P_WIDTH, 0.0),
    ]),

    Preset::from_overrides("Wide (×1.5)", &[
        (P_WIDTH, 1.5),
    ]),

    Preset::from_overrides("Super-Wide (×2)", &[
        (P_WIDTH, 2.0),
        (P_OUTPUT, -1.0),
    ]),

    Preset::from_overrides("Centre Focus (mid +3)", &[
        (P_MID, 3.0),
        (P_SIDE, -1.5),
    ]),

    Preset::from_overrides("Vocal Up-front", &[
        (P_MID, 2.5),
        (P_WIDTH, 0.85),
    ]),

    Preset::from_overrides("Mastering Encode →", &[
        (P_MODE, 1.0),
    ]),

    Preset::from_overrides("Mastering ← Decode", &[
        (P_MODE, 2.0),
    ]),
];

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
