use crate::{
    PARAMS, P_HP, P_HIGH_FREQ, P_HIGH_GAIN, P_LOW_FREQ, P_LOW_GAIN, P_MID_FREQ, P_MID_GAIN,
    P_MID_Q, P_OUTPUT,
};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Init", &[]),

    Preset::from_overrides("Master Air", &[
        (P_HIGH_FREQ, 12000.0),
        (P_HIGH_GAIN, 1.5),
        (P_HP, 30.0),
    ]),

    Preset::from_overrides("Master Warmth", &[
        (P_LOW_FREQ, 80.0),
        (P_LOW_GAIN, 1.0),
        (P_HIGH_FREQ, 10000.0),
        (P_HIGH_GAIN, 0.5),
    ]),

    Preset::from_overrides("De-mud", &[
        (P_MID_FREQ, 350.0),
        (P_MID_GAIN, -2.0),
        (P_MID_Q, 1.2),
    ]),

    Preset::from_overrides("Vocal Presence", &[
        (P_MID_FREQ, 3500.0),
        (P_MID_GAIN, 2.0),
        (P_MID_Q, 1.5),
    ]),

    Preset::from_overrides("Subwoofer Cut", &[
        (P_HP, 80.0),
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
    shared.fir_dirty.store(true, Ordering::Release);
}
