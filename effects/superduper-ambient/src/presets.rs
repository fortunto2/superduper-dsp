use crate::{P_CUTOFF, P_DRIVE, P_MODULATION, P_OUTPUT, P_RESONANCE, P_ROOT,
            P_VOICE2, P_VOICE3, P_VOICE4, P_WIDTH, PARAMS};

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

    // Cinematic bed — root A1, perfect fifth, octave, octave+5th. Wide.
    Preset::from_overrides("Cinematic Bed", &[
        (P_ROOT, 55.0),
        (P_VOICE2, 7.0),
        (P_VOICE3, 12.0),
        (P_VOICE4, 19.0),
        (P_CUTOFF, 2200.0),
        (P_RESONANCE, 0.18),
        (P_MODULATION, 10.0),
        (P_DRIVE, 0.25),
        (P_WIDTH, 12.0),
        (P_OUTPUT, -10.0),
    ]),

    // Dark drone — root very low, minor third on top, almost no drive.
    Preset::from_overrides("Dark Drone", &[
        (P_ROOT, 41.2), // E1
        (P_VOICE2, 3.0),  // minor third
        (P_VOICE3, 7.0),  // fifth
        (P_VOICE4, 15.0), // octave + minor third
        (P_CUTOFF, 800.0),
        (P_RESONANCE, 0.35),
        (P_MODULATION, 4.0),
        (P_DRIVE, 0.1),
        (P_WIDTH, 5.0),
        (P_OUTPUT, -8.0),
    ]),

    // Ethereal — high root, major triad voicing, lots of modulation,
    // bright cutoff, gentle width drift.
    Preset::from_overrides("Ethereal", &[
        (P_ROOT, 220.0), // A3
        (P_VOICE2, 4.0),   // major third
        (P_VOICE3, 7.0),   // fifth
        (P_VOICE4, 12.0),  // octave
        (P_CUTOFF, 4500.0),
        (P_RESONANCE, 0.10),
        (P_MODULATION, 18.0),
        (P_DRIVE, 0.2),
        (P_WIDTH, 18.0),
        (P_OUTPUT, -14.0),
    ]),

    // Warm bass pad — sub root, fifth on top, very mellow filter.
    Preset::from_overrides("Warm Sub", &[
        (P_ROOT, 32.7), // C1
        (P_VOICE2, 7.0),
        (P_VOICE3, 12.0),
        (P_VOICE4, 14.0), // ninth
        (P_CUTOFF, 500.0),
        (P_RESONANCE, 0.05),
        (P_MODULATION, 3.0),
        (P_DRIVE, 0.4),
        (P_WIDTH, 4.0),
        (P_OUTPUT, -6.0),
    ]),

    // Suspended chord — root + 4 + 7 + 11 (sus4 with maj7 colour).
    Preset::from_overrides("Sus Magic", &[
        (P_ROOT, 110.0), // A2
        (P_VOICE2, 5.0),  // fourth
        (P_VOICE3, 7.0),  // fifth
        (P_VOICE4, 11.0), // major seventh
        (P_CUTOFF, 2800.0),
        (P_RESONANCE, 0.15),
        (P_MODULATION, 12.0),
        (P_DRIVE, 0.2),
        (P_WIDTH, 10.0),
        (P_OUTPUT, -12.0),
    ]),

    // Solo drone (one note, no chord, useful for layering elsewhere).
    Preset::from_overrides("Solo Drone", &[
        (P_ROOT, 110.0),
        (P_VOICE2, 0.0),
        (P_VOICE3, 0.0),
        (P_VOICE4, 0.0),
        (P_CUTOFF, 1800.0),
        (P_RESONANCE, 0.25),
        (P_MODULATION, 6.0),
        (P_DRIVE, 0.3),
        (P_WIDTH, 6.0),
        (P_OUTPUT, -10.0),
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
