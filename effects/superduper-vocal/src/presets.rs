use crate::{
    P_CLK_AMT, P_CLK_FLOOR, P_CLK_SENS, P_ESS_AMT, P_ESS_FREQ, P_ESS_RANGE, P_ESS_THR,
    P_ESS_TRACK, P_MIX, P_OUTPUT, P_SUB_MODE, PARAMS,
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

    // 2-band de-esser via 2 chained instances.
    //
    // Pattern: drop the full Vocal first (any preset like Bright Vocal),
    // then add a second Vocal under it with Sib 2 selected. Sub Mode
    // makes the second instance only do its de-esser (Plos / Hum / Clk
    // / Lo are masked) so you don't double-process the cleanup stages.
    //
    // First instance — the "s" core (5.5 kHz). Fixed band, no tracker,
    // narrow Range so Sib 2 has somewhere to live without overlap.
    Preset::from_overrides("Sib 1 (s)", &[
        (P_ESS_THR, -24.0),
        (P_ESS_FREQ, 5500.0),
        (P_ESS_AMT, 6.0),
        (P_ESS_RANGE, 0.3),
        (P_ESS_TRACK, 0.0),
        (P_OUTPUT, 0.0),
        (P_MIX, 1.0),
        // Sub Mode OFF — this is the first ("upper") instance so it
        // still runs the full cleanup chain (Plos / Hum / Clk) from
        // whatever preset the user dropped on top.
    ]),

    // Second instance — the "sh" core (8 kHz). Sub Mode ON so the
    // shared cleanup chain doesn't run twice.
    Preset::from_overrides("Sib 2 (sh)", &[
        (P_ESS_THR, -22.0),
        (P_ESS_FREQ, 8000.0),
        (P_ESS_AMT, 6.0),
        (P_ESS_RANGE, 0.3),
        (P_ESS_TRACK, 0.0),
        (P_OUTPUT, 0.0),
        (P_MIX, 1.0),
        (P_SUB_MODE, 1.0),
    ]),

    // 2-band on the mastering bus — wider Range + lower thresholds for
    // the cumulative sibilance of the whole mix. Used in stereo,
    // post-limiter.
    Preset::from_overrides("Sib Master", &[
        (P_ESS_THR, -18.0),
        (P_ESS_FREQ, 7000.0),
        (P_ESS_AMT, 3.0),
        (P_ESS_RANGE, 0.6),
        (P_ESS_TRACK, 1.0),
        (P_OUTPUT, 0.0),
        (P_MIX, 1.0),
        (P_SUB_MODE, 1.0),
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
