use crate::{P_HIGH_FREQ, P_HIGH_GAIN, P_HP, P_LOW_FREQ, P_LOW_GAIN, P_LP, P_MID_FREQ, P_MID_GAIN,
            P_MID_Q, P_OUTPUT, PARAMS};

pub struct Preset {
    pub name: &'static str,
    pub values: [f32; PARAMS.len()],
}

impl Preset {
    const fn from_overrides(name: &'static str, overrides: &[(usize, f32)]) -> Self {
        let mut values = [0.0_f32; PARAMS.len()];
        let mut i = 0;
        while i < PARAMS.len() {
            values[i] = PARAMS[i].default as f32;
            i += 1;
        }
        i = 0;
        while i < overrides.len() {
            values[overrides[i].0] = overrides[i].1;
            i += 1;
        }
        Self { name, values }
    }
}

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Default", &[]),

    // Vocal sheen: gentle low rumble cut, small 3 kHz presence, air shelf.
    Preset::from_overrides("Vocal Sheen", &[
        (P_HP, 90.0),
        (P_LOW_FREQ, 200.0),
        (P_LOW_GAIN, -1.5),
        (P_MID_FREQ, 3200.0),
        (P_MID_GAIN, 2.0),
        (P_MID_Q, 1.2),
        (P_HIGH_FREQ, 10000.0),
        (P_HIGH_GAIN, 3.0),
    ]),

    // De-mud — common mix fix at 250-350 Hz boxiness.
    Preset::from_overrides("De-Mud", &[
        (P_MID_FREQ, 280.0),
        (P_MID_GAIN, -4.0),
        (P_MID_Q, 1.4),
    ]),

    // Bass weight: low shelf boost + HP at sub freqs to keep speakers happy.
    Preset::from_overrides("Bass Weight", &[
        (P_HP, 30.0),
        (P_LOW_FREQ, 80.0),
        (P_LOW_GAIN, 3.0),
        (P_MID_FREQ, 800.0),
        (P_MID_GAIN, -1.5),
        (P_MID_Q, 1.0),
    ]),

    // Drum punch: 80 Hz low shelf, 3-5 kHz attack, gentle high shelf.
    Preset::from_overrides("Drum Punch", &[
        (P_LOW_FREQ, 100.0),
        (P_LOW_GAIN, 2.5),
        (P_MID_FREQ, 4000.0),
        (P_MID_GAIN, 2.0),
        (P_MID_Q, 1.5),
        (P_HIGH_FREQ, 12000.0),
        (P_HIGH_GAIN, 1.5),
    ]),

    // Telephone — extreme tight band-pass for FX vocal.
    Preset::from_overrides("Telephone", &[
        (P_HP, 400.0),
        (P_LP, 3000.0),
        (P_MID_FREQ, 1500.0),
        (P_MID_GAIN, 4.0),
        (P_MID_Q, 0.7),
    ]),

    // Air & space — high shelf boost for sparkle on synths / pads.
    Preset::from_overrides("Air & Space", &[
        (P_HIGH_FREQ, 12000.0),
        (P_HIGH_GAIN, 4.0),
    ]),

    // Master subtle — gentle tilt: slight low boost, slight high shelf.
    Preset::from_overrides("Master Tilt", &[
        (P_LOW_FREQ, 100.0),
        (P_LOW_GAIN, 1.0),
        (P_HIGH_FREQ, 8000.0),
        (P_HIGH_GAIN, 1.5),
        (P_OUTPUT, 0.0),
    ]),
];

pub fn apply(shared: &crate::SharedParamsInner, preset: &Preset) {
    use std::sync::atomic::Ordering;
    for (i, v) in preset.values.iter().enumerate() {
        if let Some(atom) = shared.params.get(i) {
            atom.store(*v, Ordering::Relaxed);
        }
    }
}
