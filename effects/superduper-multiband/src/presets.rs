use crate::{bp, F_ATTACK, F_BYPASS, F_KEY, F_KNEE, F_MAKEUP, F_RANGE, F_RATIO, F_RELEASE, F_THRESHOLD, P_BANDS, P_XOVER, PARAMS};

superduper_dsp_sdk::define_preset!(PARAMS);

/// Bands 1..=4 as 0-based indices, for readability below.
const LOW: usize = 0;
const MID: usize = 1;
const HIGH: usize = 2;

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Default", &[]),

    // Mastering glue: three bands, the same gentle 1.5:1 everywhere, slow
    // attack so transients pass, Range 3 dB so no band ever moves far.
    Preset::from_overrides("Mastering Glue", &[
        (P_BANDS, 3.0), (P_XOVER[0], 120.0), (P_XOVER[1], 4000.0),
        (bp(LOW, F_THRESHOLD), -24.0), (bp(LOW, F_RATIO), 1.5), (bp(LOW, F_ATTACK), 30.0), (bp(LOW, F_RELEASE), 250.0), (bp(LOW, F_KNEE), 12.0), (bp(LOW, F_RANGE), 3.0),
        (bp(MID, F_THRESHOLD), -24.0), (bp(MID, F_RATIO), 1.5), (bp(MID, F_ATTACK), 30.0), (bp(MID, F_RELEASE), 200.0), (bp(MID, F_KNEE), 12.0), (bp(MID, F_RANGE), 3.0),
        (bp(HIGH, F_THRESHOLD), -26.0), (bp(HIGH, F_RATIO), 1.5), (bp(HIGH, F_ATTACK), 20.0), (bp(HIGH, F_RELEASE), 150.0), (bp(HIGH, F_KNEE), 12.0), (bp(HIGH, F_RANGE), 3.0),
    ]),

    // De-mud: only 200-500 Hz reacts, and only when it builds up.
    Preset::from_overrides("De-Mud Low-Mid", &[
        (P_BANDS, 3.0), (P_XOVER[0], 200.0), (P_XOVER[1], 500.0),
        (bp(LOW, F_BYPASS), 1.0), (bp(HIGH, F_BYPASS), 1.0),
        (bp(MID, F_THRESHOLD), -30.0), (bp(MID, F_RATIO), 3.0), (bp(MID, F_ATTACK), 10.0),
        (bp(MID, F_RELEASE), 120.0), (bp(MID, F_KNEE), 6.0), (bp(MID, F_RANGE), 6.0),
    ]),

    // Vocal de-harsh: 2-5 kHz, fast, capped at 8 dB.
    Preset::from_overrides("Vocal De-Harsh", &[
        (P_BANDS, 3.0), (P_XOVER[0], 2000.0), (P_XOVER[1], 5000.0),
        (bp(LOW, F_BYPASS), 1.0), (bp(HIGH, F_BYPASS), 1.0),
        (bp(MID, F_THRESHOLD), -32.0), (bp(MID, F_RATIO), 4.0), (bp(MID, F_ATTACK), 2.0),
        (bp(MID, F_RELEASE), 80.0), (bp(MID, F_KNEE), 6.0), (bp(MID, F_RANGE), 8.0),
    ]),

    // Duck under key: pick the vocal (or kick) as the bus key; the mid band
    // (250 Hz-4 kHz) drops up to 6 dB while the key is loud *in that range*,
    // lows and highs compress gently on their own signal. The key level is
    // band RMS on the bus scale (−50 dB is the bus's "loud" line), so the
    // threshold sits just under it.
    Preset::from_overrides("Duck Under Key", &[
        (P_BANDS, 3.0), (P_XOVER[0], 250.0), (P_XOVER[1], 4000.0),
        (bp(MID, F_KEY), 100.0), (bp(MID, F_THRESHOLD), -55.0), (bp(MID, F_RATIO), 4.0),
        (bp(MID, F_ATTACK), 5.0), (bp(MID, F_RELEASE), 180.0), (bp(MID, F_KNEE), 6.0), (bp(MID, F_RANGE), 6.0),
        (bp(LOW, F_THRESHOLD), -20.0), (bp(LOW, F_RATIO), 1.5), (bp(LOW, F_RANGE), 3.0),
        (bp(HIGH, F_THRESHOLD), -20.0), (bp(HIGH, F_RATIO), 1.5), (bp(HIGH, F_RANGE), 3.0),
    ]),

    // Bass control: two bands, hold the low end steady under 120 Hz.
    Preset::from_overrides("Bass Control", &[
        (P_BANDS, 2.0), (P_XOVER[0], 120.0),
        (bp(LOW, F_THRESHOLD), -22.0), (bp(LOW, F_RATIO), 4.0), (bp(LOW, F_ATTACK), 20.0),
        (bp(LOW, F_RELEASE), 150.0), (bp(LOW, F_KNEE), 6.0), (bp(LOW, F_MAKEUP), 1.0), (bp(LOW, F_RANGE), 8.0),
        (bp(MID, F_BYPASS), 1.0),
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
