use crate::{PARAMS, P_BREATH, P_MODE, P_MORPH, P_SOURCE, P_TARGET, P_TILT, P_VOWELS};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Init", &[]),
    // The whole way: the voice plays a Bashkir kubyz.
    Preset::from_overrides("Voice → Kubyz", &[(P_MORPH, 1.0), (P_SOURCE, 2.0), (P_TARGET, 1.0), (P_BREATH, 0.2)]),
    // Kubyz harmonics, the voice's mouth: the jaw harp says the words.
    Preset::from_overrides("Talking Kubyz", &[(P_MORPH, 1.0), (P_VOWELS, 0.8), (P_SOURCE, 2.0), (P_TARGET, 1.0)]),
    // Halfway: a voice with a metallic, whistling edge.
    Preset::from_overrides("Half Kubyz", &[(P_MORPH, 0.5), (P_SOURCE, 2.0), (P_TARGET, 3.0), (P_BREATH, 0.6)]),
    // Reed instead of a jaw harp.
    Preset::from_overrides("Voice → Aulos", &[(P_MORPH, 1.0), (P_SOURCE, 2.0), (P_TARGET, 4.0), (P_TILT, -1.0)]),
    // Whatever plays on the Target input (track channels 3/4).
    Preset::from_overrides("Sidechain Morph", &[(P_MORPH, 1.0), (P_SOURCE, 0.0)]),
    // The snapshot taken with Capture.
    Preset::from_overrides("Captured Morph", &[(P_MORPH, 1.0), (P_SOURCE, 1.0)]),
    // Envelope transfer — drums, noise, anything without a pitch.
    Preset::from_overrides("Envelope Morph", &[(P_MORPH, 1.0), (P_SOURCE, 0.0), (P_MODE, 1.0)]),
];

pub fn apply(shared: &crate::SharedParamsInner, preset: &Preset) {
    for (i, v) in preset.values.iter().enumerate() {
        shared.write_param(i, *v);
    }
}
