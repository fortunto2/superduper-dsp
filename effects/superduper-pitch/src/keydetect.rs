//! Musical key detection (Krumhansl-Schmuckler).
//!
//! Accumulates a **chromagram** (12 pitch-class energies) from its own STFT of
//! the incoming audio, then correlates it against the 24 Krumhansl-Kessler key
//! profiles (12 major + 12 minor) to name the key — e.g. "A minor". Runs in
//! both engine modes (its own small FFT), publishes the result for the GUI.
//!
//! RT-safe: the realfft plan and all buffers are allocated in [`new`];
//! `push`/`analyze` never allocate.

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

const N: usize = 4096;
const HOP: usize = 2048;
const HALF: usize = N / 2;

/// Krumhansl-Kessler major/minor key profiles (relative tonal hierarchy).
const MAJOR: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

/// `key < 24` = detected (0..11 major C..B, 12..23 minor C..B). `key == 24` =
/// none (silence / unclear).
pub const KEY_NONE: usize = 24;

pub struct KeyDetector {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Box<[f32]>,
    ring: Box<[f32]>,
    write: usize,
    filled: usize,
    hop_count: usize,
    fft_in: Box<[f32]>,
    spectrum: Box<[Complex<f32>]>,
    scratch: Box<[Complex<f32>]>,
    /// Pitch class (0..11) for each bin, or -1 to skip.
    bin_pc: Box<[i8]>,
    chroma: [f32; 12],
    key: usize,
    conf: f32,
}

impl KeyDetector {
    pub fn new(sr: f32) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(N);
        let scratch = fft.make_scratch_vec().into_boxed_slice();
        let window: Box<[f32]> = (0..N)
            .map(|k| 0.5 - 0.5 * (core::f32::consts::TAU * k as f32 / N as f32).cos())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        // Map each bin to a pitch class (skip sub-bass rumble + high noise).
        let bin_pc: Box<[i8]> = (0..=HALF)
            .map(|k| {
                let f = k as f32 * sr / N as f32;
                if f < 55.0 || f > 5000.0 {
                    -1i8
                } else {
                    let midi = 69.0 + 12.0 * (f / 440.0).log2();
                    (midi.round() as i32).rem_euclid(12) as i8
                }
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            fft,
            window,
            ring: vec![0.0; N].into_boxed_slice(),
            write: 0,
            filled: 0,
            hop_count: 0,
            fft_in: vec![0.0; N].into_boxed_slice(),
            spectrum: vec![Complex::new(0.0, 0.0); HALF + 1].into_boxed_slice(),
            scratch,
            bin_pc,
            chroma: [0.0; 12],
            key: KEY_NONE,
            conf: 0.0,
        }
    }

    pub fn key(&self) -> usize {
        self.key
    }
    pub fn confidence(&self) -> f32 {
        self.conf
    }

    /// Feed one (mono) input sample.
    #[inline]
    pub fn push(&mut self, x: f32) {
        self.ring[self.write] = x;
        self.write += 1;
        if self.write >= N {
            self.write = 0;
        }
        if self.filled < N {
            self.filled += 1;
        }
        self.hop_count += 1;
        if self.hop_count >= HOP && self.filled >= N {
            self.hop_count = 0;
            self.analyze();
        }
    }

    fn analyze(&mut self) {
        for i in 0..N {
            let idx = self.write + i;
            let idx = if idx >= N { idx - N } else { idx };
            self.fft_in[i] = self.ring[idx] * self.window[i];
        }
        let _ = self
            .fft
            .process_with_scratch(&mut self.fft_in, &mut self.spectrum, &mut self.scratch);

        let mut frame = [0.0f32; 12];
        for k in 0..=HALF {
            let pc = self.bin_pc[k];
            if pc >= 0 {
                let c = self.spectrum[k];
                frame[pc as usize] += (c.re * c.re + c.im * c.im).sqrt();
            }
        }
        // Rolling chromagram (~0.9 s memory at this hop).
        for i in 0..12 {
            self.chroma[i] = self.chroma[i] * 0.85 + frame[i] * 0.15;
        }
        self.detect();
    }

    fn detect(&mut self) {
        let total: f32 = self.chroma.iter().sum();
        if total < 1e-4 {
            self.key = KEY_NONE;
            self.conf = 0.0;
            return;
        }
        let mut c = [0.0f32; 12];
        for i in 0..12 {
            c[i] = self.chroma[i] / total;
        }
        let mut best_key = KEY_NONE;
        let mut best_r = -2.0f32;
        for (mode, prof) in [MAJOR, MINOR].iter().enumerate() {
            for tonic in 0..12 {
                let r = pearson_rotated(&c, prof, tonic);
                if r > best_r {
                    best_r = r;
                    best_key = mode * 12 + tonic;
                }
            }
        }
        self.key = best_key;
        self.conf = best_r.max(0.0);
    }
}

/// Pearson correlation between `c` and `prof` rotated so `prof[0]` lands on
/// pitch class `tonic`.
fn pearson_rotated(c: &[f32; 12], prof: &[f32; 12], tonic: usize) -> f32 {
    let mut p = [0.0f32; 12];
    for i in 0..12 {
        p[i] = prof[(i + 12 - tonic) % 12];
    }
    let mc: f32 = c.iter().sum::<f32>() / 12.0;
    let mp: f32 = p.iter().sum::<f32>() / 12.0;
    let mut num = 0.0f32;
    let mut dc = 0.0f32;
    let mut dp = 0.0f32;
    for i in 0..12 {
        let a = c[i] - mc;
        let b = p[i] - mp;
        num += a * b;
        dc += a * a;
        dp += b * b;
    }
    let den = (dc * dp).sqrt();
    if den > 1e-9 {
        num / den
    } else {
        0.0
    }
}

/// "C major" / "A minor" for a key index 0..23, or "—" for none.
pub fn key_name(key: usize) -> &'static str {
    const NAMES: [&str; 25] = [
        "C major", "C# major", "D major", "D# major", "E major", "F major", "F# major",
        "G major", "G# major", "A major", "A# major", "B major", "C minor", "C# minor",
        "D minor", "D# minor", "E minor", "F minor", "F# minor", "G minor", "G# minor",
        "A minor", "A# minor", "B minor", "—",
    ];
    NAMES[key.min(24)]
}

/// Tonic pitch class (0..11) of a key index, or None for KEY_NONE.
pub fn key_tonic(key: usize) -> Option<usize> {
    if key >= KEY_NONE {
        None
    } else {
        Some(key % 12)
    }
}

/// Nearest-octave semitone shift (−6..+6) that moves `detected`'s tonic onto
/// `target`'s tonic. `None` if either key is undetected/None. This is the
/// "Match" action: transpose this track into the target key.
pub fn match_interval(detected: usize, target: usize) -> Option<i32> {
    let dt = key_tonic(detected)? as i32;
    let tt = key_tonic(target)? as i32;
    let mut iv = (((tt - dt) % 12) + 12) % 12;
    if iv > 6 {
        iv -= 12;
    }
    Some(iv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_interval_nearest_octave() {
        // C major (0) → A major (9): −3 semitones (nearest octave), not +9.
        assert_eq!(match_interval(0, 9), Some(-3));
        // C major → D major (2): +2.
        assert_eq!(match_interval(0, 2), Some(2));
        // C major → F# major (6): +6 (tritone, either direction).
        assert_eq!(match_interval(0, 6), Some(6));
        // Same key → 0.
        assert_eq!(match_interval(0, 0), Some(0));
        // Minor target: A minor tonic (21 → tonic 9). C major → −3.
        assert_eq!(match_interval(0, 21), Some(-3));
        // None cases.
        assert_eq!(match_interval(KEY_NONE, 5), None);
        assert_eq!(match_interval(3, 0), Some(-3));
    }
}
