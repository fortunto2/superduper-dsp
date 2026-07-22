//! Factory presets for SuperDuper Tune.

use crate::{
    P_AMOUNT, P_FORMANT, P_RETUNE, P_SCALE, P_TARGET, PARAMS, TARGET_MIDI, TARGET_SIDECHAIN,
};

superduper_dsp_sdk::define_preset!(PARAMS);

// Scale indices into `scale::SCALES`.
const SCALE_CHROMATIC: f32 = 0.0;
const SCALE_MAJOR: f32 = 1.0;
const SCALE_MINOR: f32 = 2.0;
const SCALE_MIN_PENTA: f32 = 8.0;

pub static PRESETS: &[Preset] = &[
    // Instant snap to a major scale — the classic hard-tune / T-Pain effect.
    Preset::from_overrides("Hard Tune", &[
        (P_SCALE, SCALE_MAJOR),
        (P_RETUNE, 0.0),
        (P_AMOUNT, 1.0),
    ]),

    // Transparent correction — a gliding retune that fixes intonation without
    // the obvious autotune artefact.
    Preset::from_overrides("Natural", &[
        (P_SCALE, SCALE_MAJOR),
        (P_RETUNE, 120.0),
        (P_AMOUNT, 0.85),
    ]),

    // Just nudge flat/sharp notes home — light touch.
    Preset::from_overrides("Subtle Correct", &[
        (P_SCALE, SCALE_MAJOR),
        (P_RETUNE, 200.0),
        (P_AMOUNT, 0.5),
    ]),

    // Minor-key hard tune for darker material.
    Preset::from_overrides("Minor Hard", &[
        (P_SCALE, SCALE_MINOR),
        (P_RETUNE, 0.0),
        (P_AMOUNT, 1.0),
    ]),

    // Snap to every semitone, instantly → stepped, robotic pitch.
    Preset::from_overrides("Robot", &[
        (P_SCALE, SCALE_CHROMATIC),
        (P_RETUNE, 0.0),
        (P_AMOUNT, 1.0),
    ]),

    // Pentatonic keeps it always-musical — great for improv / riffing.
    Preset::from_overrides("Pentatonic", &[
        (P_SCALE, SCALE_MIN_PENTA),
        (P_RETUNE, 30.0),
        (P_AMOUNT, 1.0),
    ]),

    // Play the melody on a MIDI keyboard; the voice is pulled to the keys.
    Preset::from_overrides("MIDI Graph", &[
        (P_TARGET, TARGET_MIDI as f32),
        (P_RETUNE, 0.0),
        (P_AMOUNT, 1.0),
    ]),

    // Follow the pitch of a reference on the sidechain (sing to a synth line).
    Preset::from_overrides("Sidechain Follow", &[
        (P_TARGET, TARGET_SIDECHAIN as f32),
        (P_RETUNE, 40.0),
        (P_AMOUNT, 1.0),
    ]),

    // Creative body-shift — hard tune with formants raised for a bright,
    // small-throat character.
    Preset::from_overrides("Bright Doll", &[
        (P_SCALE, SCALE_MAJOR),
        (P_RETUNE, 0.0),
        (P_AMOUNT, 1.0),
        (P_FORMANT, 4.0),
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
