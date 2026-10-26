use crate::{PARAMS, P_AMOUNT, P_MIX, P_MODE, P_REDUCTION, P_RT60, P_SMOOTHING, P_THRESHOLD};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Init", &[]),

    // Gentle: hiss under a voice-over, nothing audible gating.
    Preset::from_overrides("Light Hiss", &[
        (P_REDUCTION, 8.0),
        (P_THRESHOLD, 4.0),
        (P_SMOOTHING, 70.0),
    ]),

    // Vlog outdoors: wind bed and traffic between phrases.
    Preset::from_overrides("Vlog Street", &[
        (P_REDUCTION, 18.0),
        (P_THRESHOLD, 6.0),
        (P_SMOOTHING, 50.0),
    ]),

    // Heavy: fan or AC room tone, accept some artefacts.
    Preset::from_overrides("Room Tone Kill", &[
        (P_REDUCTION, 30.0),
        (P_THRESHOLD, 8.0),
        (P_SMOOTHING, 40.0),
    ]),

    // Voice-only cleanup on a 48 kHz session.
    Preset::from_overrides("Neural Voice", &[
        (P_MODE, 2.0),
    ]),

    // Same, with some room left in so it does not sound sealed.
    Preset::from_overrides("Neural Voice Gentle", &[
        (P_MODE, 2.0),
        (P_MIX, 70.0),
    ]),

    Preset::from_overrides("Dereverb Small Room", &[
        (P_MODE, 1.0),
        (P_AMOUNT, 50.0),
        (P_RT60, 0.5),
    ]),

    Preset::from_overrides("Dereverb Hall", &[
        (P_MODE, 1.0),
        (P_AMOUNT, 80.0),
        (P_RT60, 1.6),
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
