use crate::{PARAMS, P_DRIVE, P_INPUT, P_MIX, P_OUTPUT, P_TONE};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Init", &[]),

    // Clean — let the network barely break a sweat. Input low, tone flat.
    Preset::from_overrides("Clean Preamp", &[
        (P_INPUT, 0.0),
        (P_DRIVE, 1.0),
        (P_OUTPUT, 0.0),
        (P_MIX, 1.0),
        (P_TONE, 0.0),
    ]),

    // Warm tube — moderate drive, slightly dark tone (tone -0.2 → low shelf up).
    Preset::from_overrides("Warm Tube", &[
        (P_INPUT, 3.0),
        (P_DRIVE, 4.0),
        (P_OUTPUT, -3.0),
        (P_MIX, 1.0),
        (P_TONE, -0.15),
    ]),

    // Crunch — heavy drive, bright tone. Network in saturation range.
    Preset::from_overrides("Crunch", &[
        (P_INPUT, 6.0),
        (P_DRIVE, 7.0),
        (P_OUTPUT, -6.0),
        (P_MIX, 1.0),
        (P_TONE, 0.2),
    ]),

    // Vocal warmth — gentle, mostly dry. Use as a subtle "console preamp"
    // colour on dialogue / vocal stems.
    Preset::from_overrides("Vocal Warmth", &[
        (P_INPUT, 1.0),
        (P_DRIVE, 2.0),
        (P_OUTPUT, 0.0),
        (P_MIX, 0.5),
        (P_TONE, -0.05),
    ]),

    // Bass thickener — focus on the lower-mid harmonics. Tone darker so
    // the added grit lives where the bass body is.
    Preset::from_overrides("Bass Thicken", &[
        (P_INPUT, 2.0),
        (P_DRIVE, 5.0),
        (P_OUTPUT, -4.0),
        (P_MIX, 0.7),
        (P_TONE, -0.3),
    ]),

    // Heavy — maximum drive for guitar-style distortion. Likely too much
    // on vocals; great on bass / synth leads.
    Preset::from_overrides("Heavy Drive", &[
        (P_INPUT, 9.0),
        (P_DRIVE, 10.0),
        (P_OUTPUT, -9.0),
        (P_MIX, 1.0),
        (P_TONE, 0.1),
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
