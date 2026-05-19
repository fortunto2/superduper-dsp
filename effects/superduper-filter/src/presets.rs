//! Factory presets — Daft Punk filter sweeps + classic auto-wah +
//! mastering-friendly broad LP / HP combos. Curated to be useful on
//! the master bus, drum bus, or any audio you want to bend.

use crate::{
    PARAMS, P_CUTOFF, P_DRIVE, P_DRV_TYPE, P_ENV_DPT, P_ENV_REL, P_LFO_DIV, P_LFO_DPT,
    P_LFO_RATE, P_LFO_SHP, P_LFO_SYNC, P_MIX, P_OUTPUT, P_RESO, P_TYPE,
};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Init", &[]),

    // ----- Daft Punk territory --------------------------------------------

    // "Around the World" / "One More Time" — slow LP sweep open-to-close
    // tied to host BPM. The LFO ride covers ~5 octaves over 8 bars.
    Preset::from_overrides("DP Sweep 8-bar (Master)", &[
        (P_TYPE, 0.0),
        (P_CUTOFF, 105.0),         // ~7 kHz — start fully open
        (P_RESO, 0.55),            // touch of squelch
        (P_DRIVE, 0.0),
        (P_LFO_SYNC, 1.0),
        (P_LFO_DIV, 1.0),          // 1/1 — one cycle per bar; tap to 8 for slower
        (P_LFO_SHP, 0.0),          // sine
        (P_LFO_DPT, -36.0),        // ramp DOWN three octaves
        (P_MIX, 1.0),
        (P_OUTPUT, 0.0),
    ]),

    // "Lose Yourself to Dance" guitar-bass interaction — heavier resonance,
    // saturate the filter to bring out the squelch character.
    Preset::from_overrides("French House Squelch", &[
        (P_TYPE, 0.0),
        (P_CUTOFF, 78.0),
        (P_RESO, 0.82),
        (P_DRIVE, 0.35),
        (P_DRV_TYPE, 3.0),         // Tube — asymmetric, 2nd harmonic
        (P_LFO_SYNC, 1.0),
        (P_LFO_DIV, 3.0),          // 1/2
        (P_LFO_SHP, 0.0),
        (P_LFO_DPT, -20.0),
        (P_MIX, 1.0),
    ]),

    // ----- Auto-wah / envelope-followed filter -----------------------------

    Preset::from_overrides("Auto-Wah Bass", &[
        (P_TYPE, 0.0),
        (P_CUTOFF, 50.0),
        (P_RESO, 0.7),
        (P_DRIVE, 0.2),
        (P_DRV_TYPE, 1.0),         // Tanh
        (P_ENV_DPT, 36.0),         // env opens cutoff by 3 oct on hit
        (P_ENV_REL, 80.0),
        (P_MIX, 1.0),
    ]),

    Preset::from_overrides("Talk-Box BP", &[
        (P_TYPE, 2.0),             // BP
        (P_CUTOFF, 75.0),
        (P_RESO, 0.85),
        (P_ENV_DPT, 24.0),
        (P_ENV_REL, 60.0),
        (P_MIX, 0.8),
    ]),

    // ----- "Telephone" lo-fi narrow band -----------------------------------

    Preset::from_overrides("Lo-Fi Telephone (HP→BP)", &[
        (P_TYPE, 2.0),             // BP
        (P_CUTOFF, 95.0),
        (P_RESO, 0.25),
        (P_DRIVE, 0.5),
        (P_DRV_TYPE, 2.0),         // Tape
        (P_MIX, 1.0),
        (P_OUTPUT, 4.0),           // BP loses energy — bump back up
    ]),

    // ----- Mastering-friendly broad LP / HP --------------------------------

    Preset::from_overrides("Master LP (gentle dome)", &[
        (P_TYPE, 0.0),
        (P_CUTOFF, 122.0),         // ~14 kHz — air taper
        (P_RESO, 0.0),
        (P_DRIVE, 0.0),
        (P_MIX, 1.0),
    ]),

    Preset::from_overrides("Master HP @ 30 Hz", &[
        (P_TYPE, 1.0),
        (P_CUTOFF, 22.0),          // ~30 Hz — sub rumble cut
        (P_RESO, 0.0),
        (P_DRIVE, 0.0),
        (P_MIX, 1.0),
    ]),

    // ----- Trance gate / synth wobble --------------------------------------

    Preset::from_overrides("Wobble Bass (Square 1/8)", &[
        (P_TYPE, 0.0),
        (P_CUTOFF, 72.0),
        (P_RESO, 0.8),
        (P_DRIVE, 0.4),
        (P_DRV_TYPE, 3.0),         // Tube
        (P_LFO_SYNC, 1.0),
        (P_LFO_DIV, 6.0),          // 1/8
        (P_LFO_SHP, 3.0),          // square
        (P_LFO_DPT, 30.0),
        (P_MIX, 1.0),
    ]),

    // ----- Notch — surgical sweep, sounds like a phaser when LFO is on ------

    Preset::from_overrides("Notch Phaser (Free Sine)", &[
        (P_TYPE, 3.0),             // Notch
        (P_CUTOFF, 90.0),
        (P_RESO, 0.5),
        (P_LFO_RATE, 0.7),
        (P_LFO_SHP, 0.0),
        (P_LFO_DPT, 18.0),
        (P_MIX, 0.6),
    ]),
];

/// Apply a preset — store each value into the matching atomic and
/// mark dirty so the host's FX automation lane records the change.
pub fn apply(shared: &crate::SharedParamsInner, preset: &Preset) {
    use std::sync::atomic::Ordering;
    for (i, v) in preset.values.iter().enumerate() {
        if let Some(atom) = shared.params.get(i) {
            atom.store(*v, Ordering::Relaxed);
        }
        if let Some(d) = shared.dirty_params.get(i) {
            d.store(true, Ordering::Relaxed);
        }
    }
}
