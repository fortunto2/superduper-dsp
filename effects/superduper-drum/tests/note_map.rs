//! The kit must answer to GM patterns as well as to its own white-key layout.
//!
//! Regression: before `NoteMap` existed the kit understood white keys only, so
//! a GM drum pattern — the format every loop pack, DAW export and hand-written
//! generator uses — lost its hats (42/46) and its clap (39) in silence. Two
//! finished tracks rendered with no top end before anyone noticed.

use superduper_drum::voices::{
    NoteMap, VoiceKind, note_to_voice, note_to_voice_auto, note_to_voice_gm, note_to_voice_with,
};

#[test]
fn gm_pattern_reaches_every_voice_we_model() {
    // The notes a GM pattern actually uses for our six voices.
    assert_eq!(note_to_voice_gm(36), Some(VoiceKind::Kick));
    assert_eq!(note_to_voice_gm(35), Some(VoiceKind::Kick));
    assert_eq!(note_to_voice_gm(38), Some(VoiceKind::Snare));
    assert_eq!(note_to_voice_gm(40), Some(VoiceKind::Snare));
    assert_eq!(note_to_voice_gm(39), Some(VoiceKind::Clap));
    assert_eq!(note_to_voice_gm(42), Some(VoiceKind::HatClosed));
    assert_eq!(note_to_voice_gm(44), Some(VoiceKind::HatClosed));
    assert_eq!(note_to_voice_gm(46), Some(VoiceKind::HatOpen));
    assert_eq!(note_to_voice_gm(56), Some(VoiceKind::Cowbell));
}

#[test]
fn gm_notes_for_instruments_we_lack_stay_unmapped() {
    // Toms, ride, crash, tambourine: no voice, so they pass through to the
    // note output for a chained synth rather than triggering something wrong.
    for key in [41, 43, 45, 47, 48, 49, 51, 54] {
        assert_eq!(note_to_voice_gm(key), None, "GM {key} should not map");
    }
}

#[test]
fn white_key_layout_is_unchanged() {
    // Old behaviour, still reachable explicitly — a player sweeping C-A.
    assert_eq!(note_to_voice(60), Some(VoiceKind::Kick));       // C4
    assert_eq!(note_to_voice(62), Some(VoiceKind::Snare));      // D4
    assert_eq!(note_to_voice(64), Some(VoiceKind::HatClosed));  // E4
    assert_eq!(note_to_voice(65), Some(VoiceKind::HatOpen));    // F4
    assert_eq!(note_to_voice(67), Some(VoiceKind::Clap));       // G4
    assert_eq!(note_to_voice(69), Some(VoiceKind::Cowbell));    // A4
    assert_eq!(note_to_voice(61), None);                        // C#4 → passthrough
}

#[test]
fn auto_prefers_gm_inside_the_percussion_octave() {
    // 42 is nothing on the white-key map and a closed hat in GM.
    assert_eq!(note_to_voice_auto(42), Some(VoiceKind::HatClosed));
    assert_eq!(note_to_voice_auto(39), Some(VoiceKind::Clap));
    // The single genuine collision: 40 is GM Electric Snare, white-key E.
    // GM wins inside its own octave.
    assert_eq!(note_to_voice_auto(40), Some(VoiceKind::Snare));
    assert_eq!(note_to_voice(40), Some(VoiceKind::HatClosed));
}

#[test]
fn auto_keeps_the_white_key_layout_outside_gm_range() {
    // Everything an actual player touches is above the GM percussion octave,
    // so the keyboard layout survives the change untouched.
    for (key, want) in [
        (60, VoiceKind::Kick),
        (64, VoiceKind::HatClosed),
        (65, VoiceKind::HatOpen),
        (67, VoiceKind::Clap),
        (72, VoiceKind::Kick),
        (76, VoiceKind::HatClosed),
    ] {
        assert_eq!(note_to_voice_auto(key), Some(want), "key {key}");
    }
}

#[test]
fn note_map_param_selects_the_layout() {
    assert_eq!(NoteMap::from_param(0.0), NoteMap::Auto);
    assert_eq!(NoteMap::from_param(1.0), NoteMap::WhiteKeys);
    assert_eq!(NoteMap::from_param(2.0), NoteMap::Gm);
    assert_eq!(NoteMap::from_param(1.4), NoteMap::WhiteKeys); // rounds

    assert_eq!(note_to_voice_with(NoteMap::WhiteKeys, 42), None);
    assert_eq!(note_to_voice_with(NoteMap::Gm, 42), Some(VoiceKind::HatClosed));
    assert_eq!(note_to_voice_with(NoteMap::Gm, 64), None); // no white-key fallback
    assert_eq!(note_to_voice_with(NoteMap::Auto, 64), Some(VoiceKind::HatClosed));
}

#[test]
fn the_beat_that_broke_renders_now_plays_in_full() {
    // The exact pattern from demos7/build_track.py, as GM. Every one of these
    // used to be silent except kick and snare.
    let gm_break = [
        (36, VoiceKind::Kick),
        (38, VoiceKind::Snare),
        (42, VoiceKind::HatClosed),
        (46, VoiceKind::HatOpen),
        (39, VoiceKind::Clap),
    ];
    for (key, want) in gm_break {
        assert_eq!(
            note_to_voice_with(NoteMap::Auto, key),
            Some(want),
            "GM note {key} must reach {want:?} under the default layout"
        );
    }
}
