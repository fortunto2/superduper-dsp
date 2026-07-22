//! Scale / key quantiser for the autotune correction.
//!
//! A scale is a 12-bit mask: bit `n` set means semitone `n` **above the key
//! root** is an allowed note. `nearest_correction_st` snaps a measured pitch to
//! the closest allowed note and returns the correction in semitones (what to add
//! to the input pitch). That semitone value is what drives the PSOLA shifter.

/// Factory scales — name + 12-bit degree mask (bit `n` = semitone `n` above the
/// root is in the scale). Decimal so the degree list can't drift from the bits.
pub const SCALES: &[(&str, u16)] = &[
    ("Chromatic", 4095),  // 0 1 2 3 4 5 6 7 8 9 10 11
    ("Major", 2741),      // 0 2 4 5 7 9 11
    ("Minor", 1453),      // 0 2 3 5 7 8 10  (natural minor)
    ("Dorian", 1709),     // 0 2 3 5 7 9 10
    ("Phrygian", 1451),   // 0 1 3 5 7 8 10
    ("Mixolydian", 1717), // 0 2 4 5 7 9 10
    ("Harm Minor", 2477), // 0 2 3 5 7 8 11
    ("Maj Penta", 661),   // 0 2 4 7 9
    ("Min Penta", 1193),  // 0 3 5 7 10
];

pub const NUM_SCALES: usize = SCALES.len();

/// Note names for the Key selector.
pub const KEY_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Hz → MIDI note number (float). A4 (69) = 440 Hz.
#[inline]
pub fn hz_to_midi(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

/// MIDI note number (float) → Hz.
#[inline]
pub fn midi_to_hz(m: f32) -> f32 {
    440.0 * 2f32.powf((m - 69.0) / 12.0)
}

/// Semitone correction that snaps `f0` (Hz) to the nearest note allowed by
/// `key` (0..11, 0 = C) and `mask`. Returns `target_midi - input_midi`, i.e.
/// how many semitones to shift up (+) or down (−). Empty/degenerate masks fall
/// back to chromatic (snap to nearest semitone).
pub fn nearest_correction_st(f0: f32, key: u8, mask: u16) -> f32 {
    if f0 <= 0.0 {
        return 0.0;
    }
    let mask = if mask & 0x0FFF == 0 { 0x0FFF } else { mask };
    let midi = hz_to_midi(f0);
    let lo = midi.floor() as i32 - 1;
    let hi = midi.ceil() as i32 + 1;
    let key = key as i32 % 12;
    let mut best_m = midi.round() as i32;
    let mut best_d = f32::INFINITY;
    for m in lo..=hi {
        let degree = ((m - key) % 12 + 12) % 12;
        if mask & (1u16 << degree) != 0 {
            let d = (m as f32 - midi).abs();
            if d < best_d {
                best_d = d;
                best_m = m;
            }
        }
    }
    best_m as f32 - midi
}

/// Correction that snaps `f0` to a specific target Hz (MIDI mode: the held key;
/// Sidechain mode: the reference pitch). Returns semitones, 0 if either is
/// silent.
#[inline]
pub fn correction_to_hz(f0: f32, target_hz: f32) -> f32 {
    if f0 <= 0.0 || target_hz <= 0.0 {
        0.0
    } else {
        12.0 * (target_hz / f0).log2()
    }
}
