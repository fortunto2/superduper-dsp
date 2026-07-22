//! Factory presets for SuperDuper Pitch.

use crate::{MODE_TRACK, P_FORMANT, P_MIX, P_MODE, P_PITCH, PARAMS};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Default", &[]),

    // Squeaky cartoon voice up an octave — with formants preserved it's "the
    // same person, higher"; nudge Formant up for the classic tape-speed squeak.
    Preset::from_overrides("Chipmunk", &[
        (P_PITCH, 12.0),
        (P_FORMANT, 0.0),
    ]),

    // Nasal cartoon (Масяня) — pitch up + formants up for a small, bright head.
    Preset::from_overrides("Masyanya", &[
        (P_PITCH, 8.0),
        (P_FORMANT, 4.0),
    ]),

    // Down an octave, formants kept → a natural, bigger voice, not a chipmunk
    // in reverse.
    Preset::from_overrides("Bass", &[
        (P_PITCH, -12.0),
        (P_FORMANT, 0.0),
    ]),

    // Menacing — down + throat lowered (formant −5) for a demon / monster.
    Preset::from_overrides("Demon", &[
        (P_PITCH, -8.0),
        (P_FORMANT, -5.0),
    ]),

    // Pitch untouched, formants up 5 st — a "gender flip" / body-size change
    // with the melody intact. The headline trick: formant independent of pitch.
    Preset::from_overrides("Gender Flip", &[
        (P_PITCH, 0.0),
        (P_FORMANT, 5.0),
    ]),

    // Gentle deepening — a few semitones down, subtle.
    Preset::from_overrides("Deeper", &[
        (P_PITCH, -4.0),
        (P_FORMANT, -2.0),
        (P_MIX, 1.0),
    ]),

    // --- Track mode: transpose whole mixes / chords / songs (phase vocoder) ---

    // Transpose a track up a whole tone.
    Preset::from_overrides("Key +2", &[
        (P_MODE, MODE_TRACK as f32),
        (P_PITCH, 2.0),
        (P_FORMANT, 0.0),
    ]),
    // ...down a whole tone.
    Preset::from_overrides("Key -2", &[
        (P_MODE, MODE_TRACK as f32),
        (P_PITCH, -2.0),
        (P_FORMANT, 0.0),
    ]),
    // Up a fourth.
    Preset::from_overrides("Key +5", &[
        (P_MODE, MODE_TRACK as f32),
        (P_PITCH, 5.0),
        (P_FORMANT, 0.0),
    ]),
    // Down a fourth.
    Preset::from_overrides("Key -5", &[
        (P_MODE, MODE_TRACK as f32),
        (P_PITCH, -5.0),
        (P_FORMANT, 0.0),
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
