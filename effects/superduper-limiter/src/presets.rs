use crate::{P_CEILING, P_INPUT, P_LOOKAHEAD, P_RELEASE, P_TRUE_PEAK, PARAMS};

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
    // Mastering — gentle, longer release, TP detector on, -1.0 dB ceiling.
    Preset::from_overrides("Mastering", &[
        (P_INPUT, 0.0),
        (P_CEILING, -1.0),
        (P_RELEASE, 80.0),
        (P_LOOKAHEAD, 5.0),
        (P_TRUE_PEAK, 1.0),
    ]),
    // Loud mastering — push input, fast release for max loudness.
    Preset::from_overrides("Loud Master", &[
        (P_INPUT, 6.0),
        (P_CEILING, -0.3),
        (P_RELEASE, 30.0),
        (P_LOOKAHEAD, 3.0),
        (P_TRUE_PEAK, 1.0),
    ]),
    // Transparent — heavy lookahead, slow release, low input drive.
    Preset::from_overrides("Transparent", &[
        (P_INPUT, 0.0),
        (P_CEILING, -0.3),
        (P_RELEASE, 150.0),
        (P_LOOKAHEAD, 8.0),
        (P_TRUE_PEAK, 1.0),
    ]),
    // Brickwall safety — final-stage broadcast limiter, -1.0 dBTP for streaming.
    Preset::from_overrides("Broadcast -1 dBTP", &[
        (P_INPUT, 0.0),
        (P_CEILING, -1.0),
        (P_RELEASE, 50.0),
        (P_LOOKAHEAD, 4.0),
        (P_TRUE_PEAK, 1.0),
    ]),
    // Drum bus tame — fast release, no TP (track use, not master).
    Preset::from_overrides("Drum Bus", &[
        (P_INPUT, 2.0),
        (P_CEILING, -0.3),
        (P_RELEASE, 15.0),
        (P_LOOKAHEAD, 1.0),
        (P_TRUE_PEAK, 0.0),
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
