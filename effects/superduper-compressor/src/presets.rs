use crate::{P_ATTACK, P_KNEE, P_MAKEUP, P_MIX, P_RATIO, P_RELEASE, P_SC_HPF, P_THRESHOLD, PARAMS};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Default", &[]),

    // Transparent vocal — gentle ratio, soft knee, medium release.
    Preset::from_overrides("Vocal Lead", &[
        (P_THRESHOLD, -18.0),
        (P_RATIO, 3.5),
        (P_ATTACK, 8.0),
        (P_RELEASE, 120.0),
        (P_KNEE, 8.0),
        (P_MAKEUP, 3.0),
        (P_SC_HPF, 1.0), // 80 Hz
        (P_MIX, 1.0),
    ]),

    // Bus glue — low ratio, slow attack, fast release, very soft knee.
    Preset::from_overrides("Bus Glue", &[
        (P_THRESHOLD, -12.0),
        (P_RATIO, 2.0),
        (P_ATTACK, 30.0),
        (P_RELEASE, 200.0),
        (P_KNEE, 12.0),
        (P_MAKEUP, 1.5),
        (P_SC_HPF, 2.0), // 150 Hz
        (P_MIX, 1.0),
    ]),

    // Drum smash — hard ratio, fast attack, fast release. Hard knee for "punch".
    Preset::from_overrides("Drum Smash", &[
        (P_THRESHOLD, -16.0),
        (P_RATIO, 8.0),
        (P_ATTACK, 1.0),
        (P_RELEASE, 60.0),
        (P_KNEE, 0.0),
        (P_MAKEUP, 4.0),
        (P_SC_HPF, 0.0),
        (P_MIX, 0.7), // NY-style parallel
    ]),

    // Bass tame — slow attack lets transient through, then squashes body.
    Preset::from_overrides("Bass Tame", &[
        (P_THRESHOLD, -20.0),
        (P_RATIO, 5.0),
        (P_ATTACK, 15.0),
        (P_RELEASE, 80.0),
        (P_KNEE, 4.0),
        (P_MAKEUP, 2.5),
        (P_SC_HPF, 0.0), // No HPF for bass — we want it triggering.
        (P_MIX, 1.0),
    ]),

    // Sidechain Pump — route kick into SC port. Heavy ratio, fast attack,
    // fast release. Mix 100% so the pump is obvious.
    Preset::from_overrides("Sidechain Pump", &[
        (P_THRESHOLD, -24.0),
        (P_RATIO, 12.0),
        (P_ATTACK, 0.5),
        (P_RELEASE, 180.0),
        (P_KNEE, 0.0),
        (P_MAKEUP, 0.0),
        (P_SC_HPF, 0.0),
        (P_MIX, 1.0),
    ]),

    // Parallel "NY" — heavy compression at 50/50 mix for thickness on drums.
    Preset::from_overrides("NY Parallel", &[
        (P_THRESHOLD, -28.0),
        (P_RATIO, 10.0),
        (P_ATTACK, 2.0),
        (P_RELEASE, 100.0),
        (P_KNEE, 2.0),
        (P_MAKEUP, 8.0),
        (P_SC_HPF, 1.0),
        (P_MIX, 0.5),
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
