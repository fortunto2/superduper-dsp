use crate::{
    P_CLK_AMT, P_CLK_FLOOR, P_CLK_SENS, P_ESS_AMT, P_ESS_FREQ, P_ESS_RANGE, P_ESS_THR, P_MIX,
    P_OUTPUT, PARAMS,
};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Init", &[]),

    // Rap vocal default — moderate de-ess at 6 kHz, light de-click for
    // mouth noise between bars.
    Preset::from_overrides("Rap Vocal", &[
        (P_ESS_THR, -22.0),
        (P_ESS_FREQ, 6000.0),
        (P_ESS_AMT, 7.0),
        (P_ESS_RANGE, 1.0),
        (P_CLK_SENS, 3.5),
        (P_CLK_AMT, 10.0),
        (P_CLK_FLOOR, -36.0),
        (P_OUTPUT, 0.0),
        (P_MIX, 1.0),
    ]),

    // Bright pop/female vocal — sibilance shifted up to 7 kHz.
    Preset::from_overrides("Bright Vocal", &[
        (P_ESS_THR, -24.0),
        (P_ESS_FREQ, 7000.0),
        (P_ESS_AMT, 8.0),
        (P_ESS_RANGE, 1.0),
        (P_CLK_SENS, 4.0),
        (P_CLK_AMT, 8.0),
        (P_CLK_FLOOR, -38.0),
        (P_OUTPUT, 0.0),
        (P_MIX, 1.0),
    ]),

    // Aggressive de-essing for over-bright recordings.
    Preset::from_overrides("Heavy De-Ess", &[
        (P_ESS_THR, -28.0),
        (P_ESS_FREQ, 5500.0),
        (P_ESS_AMT, 14.0),
        (P_ESS_RANGE, 1.0),
        (P_CLK_SENS, 8.0), // effectively off
        (P_CLK_AMT, 0.0),
        (P_CLK_FLOOR, -40.0),
        (P_OUTPUT, 1.0),
        (P_MIX, 1.0),
    ]),

    // Click-only — for cleaning dry vocal takes without touching tone.
    Preset::from_overrides("Click Only", &[
        (P_ESS_AMT, 0.0),
        (P_CLK_SENS, 3.0),
        (P_CLK_AMT, 15.0),
        (P_CLK_FLOOR, -42.0),
        (P_OUTPUT, 0.0),
        (P_MIX, 1.0),
    ]),

    // Gentle podcast vocal — light cleanup, transparent.
    Preset::from_overrides("Podcast", &[
        (P_ESS_THR, -20.0),
        (P_ESS_FREQ, 6500.0),
        (P_ESS_AMT, 4.0),
        (P_ESS_RANGE, 0.8),
        (P_CLK_SENS, 4.5),
        (P_CLK_AMT, 6.0),
        (P_CLK_FLOOR, -40.0),
        (P_OUTPUT, 0.0),
        (P_MIX, 1.0),
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
