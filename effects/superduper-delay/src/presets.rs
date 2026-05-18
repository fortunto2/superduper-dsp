use crate::{P_DRIVE, P_DUCK_AMOUNT, P_DUCK_ATTACK, P_DUCK_RELEASE, P_FEEDBACK, P_MIX,
            P_MODE, P_TIME, P_TONE, P_WIDTH, PARAMS};

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

    // 1/4 note at 120 bpm = 500 ms. Vocal slap with light feedback.
    Preset::from_overrides("Vocal Slap", &[
        (P_TIME, 90.0),
        (P_WIDTH, 0.0),
        (P_FEEDBACK, 0.18),
        (P_TONE, 4500.0),
        (P_DRIVE, 1.5),
        (P_MIX, 0.25),
        (P_MODE, 0.0),
    ]),

    // Stereo dotted-eighth (375 ms at 120 bpm). Drive subtle, dark tone.
    Preset::from_overrides("Dotted 8th", &[
        (P_TIME, 375.0),
        (P_WIDTH, 25.0),
        (P_FEEDBACK, 0.42),
        (P_TONE, 5500.0),
        (P_DRIVE, 1.2),
        (P_MIX, 0.32),
        (P_MODE, 0.0),
    ]),

    // Ping-pong quarter, heavy feedback, warm tone — classic dub.
    Preset::from_overrides("Dub Echo", &[
        (P_TIME, 500.0),
        (P_WIDTH, 0.0),
        (P_FEEDBACK, 0.72),
        (P_TONE, 2800.0),
        (P_DRIVE, 4.0),
        (P_MIX, 0.45),
        (P_MODE, 1.0), // Ping-pong
    ]),

    // Long tape wash — feedback near edge, dark, lots of drive degradation.
    Preset::from_overrides("Tape Wash", &[
        (P_TIME, 800.0),
        (P_WIDTH, -45.0),
        (P_FEEDBACK, 0.85),
        (P_TONE, 2200.0),
        (P_DRIVE, 6.0),
        (P_MIX, 0.4),
        (P_MODE, 0.0),
    ]),

    // Haas widener — short slap on R, no feedback. Use on mono sources.
    Preset::from_overrides("Haas Width", &[
        (P_TIME, 25.0),
        (P_FEEDBACK, 0.0),
        (P_TONE, 12000.0),
        (P_DRIVE, 0.0),
        (P_MIX, 0.5),
        (P_MODE, 2.0),
    ]),

    // Rhythmic ping-pong, bright, moderate feedback.
    Preset::from_overrides("Ping Bright", &[
        (P_TIME, 333.0),
        (P_FEEDBACK, 0.55),
        (P_TONE, 9000.0),
        (P_DRIVE, 0.5),
        (P_MIX, 0.38),
        (P_MODE, 1.0),
    ]),

    // The classic vocal-send-ducked preset. Route this plugin on an aux
    // send, send dry vocal there for the wet, and feed the vocal signal
    // into the Sidechain port. The delay tail ducks down whenever the
    // vocal speaks → vocal stays clear, delays fill the silences.
    // Mix = 1.0 because plug-in is on a SEND track, not insert.
    Preset::from_overrides("Vocal Send Ducked", &[
        (P_TIME, 400.0),
        (P_WIDTH, 30.0),
        (P_FEEDBACK, 0.55),
        (P_TONE, 5500.0),
        (P_DRIVE, 1.0),
        (P_MIX, 1.0),
        (P_MODE, 1.0), // Ping-pong
        (P_DUCK_AMOUNT, 12.0),
        (P_DUCK_ATTACK, 4.0),
        (P_DUCK_RELEASE, 280.0),
    ]),

    // ----- Band-flavoured presets -----
    // Joy Division — slap-back into the verb. Hooky echo with one
    // strong repeat that dies fast into the reverb.
    Preset::from_overrides("Joy Division (Slap)", &[
        (P_TIME, 200.0),
        (P_WIDTH, 20.0),
        (P_FEEDBACK, 0.32),
        (P_TONE, 3800.0),
        (P_DRIVE, 2.5),
        (P_MIX, 0.4),
        (P_MODE, 0.0),
    ]),
    // The Edge / U2 — fast dotted 8th repeats with tone rolled off.
    Preset::from_overrides("The Edge (Dotted 8th)", &[
        (P_TIME, 375.0),
        (P_WIDTH, -50.0),
        (P_FEEDBACK, 0.42),
        (P_TONE, 4800.0),
        (P_DRIVE, 1.0),
        (P_MIX, 0.4),
        (P_MODE, 0.0),
    ]),
    // Vangelis (Blade Runner) — long lush ping-pong, dark, very wet.
    Preset::from_overrides("Vangelis (Blade Runner)", &[
        (P_TIME, 600.0),
        (P_WIDTH, 100.0),
        (P_FEEDBACK, 0.55),
        (P_TONE, 3000.0),
        (P_DRIVE, 1.5),
        (P_MIX, 0.5),
        (P_MODE, 1.0),
    ]),
    // Boards of Canada — tape-style, dark feedback, lots of drive.
    Preset::from_overrides("Boards of Canada (Tape)", &[
        (P_TIME, 425.0),
        (P_WIDTH, 30.0),
        (P_FEEDBACK, 0.55),
        (P_TONE, 2400.0),
        (P_DRIVE, 6.0),
        (P_MIX, 0.4),
        (P_MODE, 0.0),
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
