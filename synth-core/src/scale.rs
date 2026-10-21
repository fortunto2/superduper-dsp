//! Key / scale quantiser — snap a measured pitch to the nearest allowed note.
//!
//! The same rule SuperDuper Tune ships (`effects/superduper-tune/src/scale.rs`),
//! lifted here so a second plugin (SuperDuper Double's Tuned mode) can use it
//! without linking another plugin's cdylib. The tables and the arithmetic are
//! identical on purpose: a Tune instance and a Double instance set to the same
//! Key and Scale must land on the same note.
// AI-NOTE: superduper-tune still carries its own copy (not edited by the Double work); point it here when next touched.
//!
//! A scale is a 12-bit mask: bit `n` set means semitone `n` above the key root
//! is allowed. [`nearest_correction_st`] returns the semitones to ADD to the
//! input pitch to reach the closest allowed note.

/// Factory scales — name + 12-bit degree mask (bit `n` = semitone `n` above
/// the root is in the scale). Same order and values as Tune's `Scale` param.
pub const SCALES: &[(&str, u16)] = &[
    ("Chromatic", 4095),  // 0 1 2 3 4 5 6 7 8 9 10 11
    ("Major", 2741),      // 0 2 4 5 7 9 11
    ("Minor", 1453),      // 0 2 3 5 7 8 10
    ("Dorian", 1709),     // 0 2 3 5 7 9 10
    ("Phrygian", 1451),   // 0 1 3 5 7 8 10
    ("Mixolydian", 1717), // 0 2 4 5 7 9 10
    ("Harm Minor", 2477), // 0 2 3 5 7 8 11
    ("Maj Penta", 661),   // 0 2 4 7 9
    ("Min Penta", 1193),  // 0 3 5 7 10
];

pub const NUM_SCALES: usize = SCALES.len();

/// Note names for a Key selector, C = 0.
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

/// Semitones that move `f0` onto the nearest note allowed by `key` (0..11) and
/// `mask`. An empty mask falls back to chromatic. 0 for a non-positive `f0`.
pub fn nearest_correction_st(f0: f32, key: u8, mask: u16) -> f32 {
    if f0 <= 0.0 || !f0.is_finite() {
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

/// Scale mask for a `Scale` param value (rounded, clamped).
pub fn scale_mask(index: f32) -> u16 {
    SCALES[(index.round().max(0.0) as usize).min(NUM_SCALES - 1)].1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a3_plus_23_cents_snaps_down_23_cents() {
        let f = 220.0 * 2f32.powf(23.0 / 1200.0);
        let c = nearest_correction_st(f, 0, SCALES[0].1);
        assert!((c * 100.0 + 23.0).abs() < 0.05, "{c}");
    }

    #[test]
    fn major_skips_out_of_scale_notes() {
        // C# (277.18 Hz) is not in C major: it goes to C or D, never stays.
        let c = nearest_correction_st(277.18, 0, SCALES[1].1);
        assert!((c.abs() - 1.0).abs() < 0.01, "{c}");
    }
}
