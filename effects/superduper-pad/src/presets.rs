use crate::{
    PARAMS, P_ATTACK, P_CUTOFF, P_DECAY, P_DRIVE, P_MODULATION, P_OUTPUT, P_RELEASE, P_RESONANCE,
    P_SUSTAIN, P_WIDTH,
};

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
    Preset::from_overrides("Init", &[]),

    // Default — broad, warm, moderate attack and tail. The "always works" patch.
    Preset::from_overrides("Pad Default", &[
        (P_CUTOFF, 3500.0),
        (P_RESONANCE, 0.18),
        (P_MODULATION, 6.0),
        (P_DRIVE, 0.20),
        (P_WIDTH, 8.0),
        (P_ATTACK, 0.4),
        (P_DECAY, 0.6),
        (P_SUSTAIN, 0.8),
        (P_RELEASE, 1.5),
        (P_OUTPUT, -8.0),
    ]),

    // Slow swell strings — long attack, sustain stays full, decay irrelevant.
    Preset::from_overrides("Slow Strings", &[
        (P_CUTOFF, 5500.0),
        (P_RESONANCE, 0.10),
        (P_MODULATION, 12.0),
        (P_DRIVE, 0.12),
        (P_WIDTH, 14.0),
        (P_ATTACK, 1.4),
        (P_DECAY, 0.8),
        (P_SUSTAIN, 0.92),
        (P_RELEASE, 2.5),
        (P_OUTPUT, -10.0),
    ]),

    // Choir — bright with breath-like modulation, instant attack.
    Preset::from_overrides("Choir", &[
        (P_CUTOFF, 6500.0),
        (P_RESONANCE, 0.06),
        (P_MODULATION, 18.0),
        (P_DRIVE, 0.18),
        (P_WIDTH, 20.0),
        (P_ATTACK, 0.05),
        (P_DECAY, 0.4),
        (P_SUSTAIN, 0.85),
        (P_RELEASE, 1.8),
        (P_OUTPUT, -10.0),
    ]),

    // Glass bells — short, hard attack, long decay to sustain, plucky feel.
    Preset::from_overrides("Glass", &[
        (P_CUTOFF, 8500.0),
        (P_RESONANCE, 0.35),
        (P_MODULATION, 4.0),
        (P_DRIVE, 0.10),
        (P_WIDTH, 6.0),
        (P_ATTACK, 0.005),
        (P_DECAY, 1.2),
        (P_SUSTAIN, 0.35),
        (P_RELEASE, 2.4),
        (P_OUTPUT, -8.0),
    ]),

    // Wide ambient pad — huge width, lots of modulation, long release.
    Preset::from_overrides("Wide Ambient", &[
        (P_CUTOFF, 2500.0),
        (P_RESONANCE, 0.22),
        (P_MODULATION, 22.0),
        (P_DRIVE, 0.25),
        (P_WIDTH, 28.0),
        (P_ATTACK, 0.8),
        (P_DECAY, 1.0),
        (P_SUSTAIN, 0.78),
        (P_RELEASE, 4.0),
        (P_OUTPUT, -10.0),
    ]),

    // Pluck — fast attack, fast decay to no sustain, short release. Stab pad.
    Preset::from_overrides("Pluck", &[
        (P_CUTOFF, 4200.0),
        (P_RESONANCE, 0.32),
        (P_MODULATION, 2.0),
        (P_DRIVE, 0.30),
        (P_WIDTH, 4.0),
        (P_ATTACK, 0.003),
        (P_DECAY, 0.35),
        (P_SUSTAIN, 0.0),
        (P_RELEASE, 0.25),
        (P_OUTPUT, -6.0),
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
