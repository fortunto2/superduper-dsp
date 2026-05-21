use crate::{P_AMOUNT, P_HI, P_LO, P_MIX, P_MODE, P_OUTPUT, P_Q, P_RELEASE, P_SENS, PARAMS};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Init", &[]),

    // Vocal sibilance + harshness — focuses on 3 kHz..10 kHz where the
    // typical "ssss / shhh / rrrr" overtones live. Sharp Q for surgical
    // cuts that don't dull the vocal.
    Preset::from_overrides("Vocal Resonance", &[
        (P_AMOUNT, 8.0),
        (P_SENS, -5.0),
        (P_Q, 6.0),
        (P_LO, 2000.0),
        (P_HI, 11000.0),
        (P_RELEASE, 60.0),
        (P_MIX, 1.0),
        (P_OUTPUT, 0.0),
        (P_MODE, 1.0),
    ]),

    // Bass guitar / kick body — kills boxy resonances in the 200-600 Hz
    // range that fight with the rest of the mix without dulling the tone.
    Preset::from_overrides("Low Mud", &[
        (P_AMOUNT, 6.0),
        (P_SENS, -7.0),
        (P_Q, 4.0),
        (P_LO, 150.0),
        (P_HI, 700.0),
        (P_RELEASE, 120.0),
        (P_MIX, 1.0),
        (P_OUTPUT, 0.0),
        (P_MODE, 0.0),
    ]),

    // Russian vocal — emphasises the F2 / F3 region where rolled-r and
    // hard sibilants resonate. Wider band + sharper Q than the default.
    Preset::from_overrides("Russian Voice", &[
        (P_AMOUNT, 10.0),
        (P_SENS, -4.0),
        (P_Q, 7.0),
        (P_LO, 1200.0),
        (P_HI, 9000.0),
        (P_RELEASE, 50.0),
        (P_MIX, 1.0),
        (P_OUTPUT, 0.0),
        (P_MODE, 1.0),
    ]),

    // Aggressive — for over-bright / nasal recordings. Heavy ratio, wide
    // catch range. Use sparingly.
    Preset::from_overrides("Tame Anything", &[
        (P_AMOUNT, 16.0),
        (P_SENS, -3.0),
        (P_Q, 5.0),
        (P_LO, 200.0),
        (P_HI, 14000.0),
        (P_RELEASE, 40.0),
        (P_MIX, 1.0),
        (P_OUTPUT, 0.0),
        (P_MODE, 2.0),
    ]),

    // Master bus polish — wide, gentle, broadband. Pulls down random
    // peaks that bypass the limiter without colouring the tone.
    Preset::from_overrides("Master Polish", &[
        (P_AMOUNT, 3.0),
        (P_SENS, -8.0),
        (P_Q, 4.0),
        (P_LO, 200.0),
        (P_HI, 14000.0),
        (P_RELEASE, 200.0),
        (P_MIX, 1.0),
        (P_OUTPUT, 0.0),
        (P_MODE, 0.0),
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
