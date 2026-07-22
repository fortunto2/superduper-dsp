//! Key-detection (Krumhansl-Schmuckler) accuracy + Match-interval tests.
//!
//! Key detection needs harmonic CONTEXT (a single triad is ambiguous — a
//! C-major triad shares notes with A minor / E minor). So the test signals are
//! I-IV-V-I chord progressions, which establish the key the way KS expects.
//!
//! Run: `cargo test -p superduper-pitch --test key_detect -- --nocapture`

use superduper_pitch::keydetect::{key_name, key_tonic, match_interval, KeyDetector};

const SR: f32 = 48_000.0;

/// Sum note frequencies with octave + fifth harmonics only (no major-third
/// harmonic, which would outline a different triad and confuse the key). `bass`
/// is a tonic-pedal frequency added under everything to assert the key.
fn chord_seg(freqs: &[f32], bass: f32, secs: f32, out: &mut Vec<f32>) {
    use std::f32::consts::TAU;
    let n = (SR * secs) as usize;
    let base = out.len();
    let harm = |f: f32, t: f32| -> f32 {
        // h = 1 (fund), 2 (octave), 3 (fifth), 4 (2 octaves).
        [1.0f32, 2.0, 3.0, 4.0]
            .iter()
            .map(|&h| {
                let fh = f * h;
                if fh < 9000.0 {
                    (1.0 / h) * (TAU * fh * t).sin()
                } else {
                    0.0
                }
            })
            .sum()
    };
    for i in 0..n {
        let t = (base + i) as f32 / SR;
        let mut s = 0.7 * harm(bass, t); // tonic pedal
        for &f in freqs {
            s += harm(f, t);
        }
        out.push(s / (freqs.len() as f32 + 1.0) * 0.35);
    }
}

/// Major triad on `root` (Hz): root, +4 semitones, +7 semitones.
fn maj(root: f32) -> [f32; 3] {
    [root, root * 2f32.powf(4.0 / 12.0), root * 2f32.powf(7.0 / 12.0)]
}

/// I–IV–V–I progression in the major key with tonic frequency `tonic`, over a
/// tonic pedal (bass = tonic an octave down), repeated so the rolling
/// chromagram settles.
fn major_progression(tonic: f32) -> Vec<f32> {
    // Octave-normalise into a range the 4096-pt FFT resolves well (low bass
    // smears the chromagram); pitch class is octave-invariant so the key holds.
    let mut tonic = tonic;
    while tonic < 330.0 {
        tonic *= 2.0;
    }
    while tonic >= 660.0 {
        tonic *= 0.5;
    }
    let i = maj(tonic);
    let iv = maj(tonic * 2f32.powf(5.0 / 12.0));
    let v = maj(tonic * 2f32.powf(7.0 / 12.0));
    let bass = tonic * 0.5;
    let mut out = Vec::new();
    for _ in 0..3 {
        chord_seg(&i, bass, 0.6, &mut out);
        chord_seg(&iv, bass, 0.6, &mut out);
        chord_seg(&v, bass, 0.6, &mut out);
        chord_seg(&i, bass, 0.6, &mut out);
    }
    out
}

fn detect(signal: &[f32]) -> usize {
    let mut kd = KeyDetector::new(SR);
    for &x in signal {
        kd.push(x);
    }
    kd.key()
}

#[test]
fn detects_c_major() {
    let key = detect(&major_progression(261.63)); // C4
    println!("C major I-IV-V-I → {} (index {key})", key_name(key));
    assert_eq!(key_tonic(key), Some(0), "expected tonic C, got {}", key_name(key));
    assert!(key < 12, "expected major, got {}", key_name(key));
}

#[test]
fn detects_transposed_key() {
    // Same progression a perfect fifth up → G major.
    let key = detect(&major_progression(392.00)); // G4
    println!("G major I-IV-V-I → {} (index {key})", key_name(key));
    assert_eq!(key_tonic(key), Some(7), "expected tonic G, got {}", key_name(key));
    assert!(key < 12, "expected major, got {}", key_name(key));
}

#[test]
fn match_moves_detected_to_target() {
    let cmaj = detect(&major_progression(261.63));
    assert_eq!(key_tonic(cmaj), Some(0));
    let target_a_major = 9usize; // key index 9 = A major
    let iv = match_interval(cmaj, target_a_major).expect("interval");
    println!("C major → A major: shift {iv:+} st");
    assert_eq!(iv, -3);

    // Transpose the whole progression by the interval and re-detect → A major.
    let f = 2f32.powf(iv as f32 / 12.0);
    let shifted = detect(&major_progression(261.63 * f));
    println!("after shift → {}", key_name(shifted));
    assert_eq!(key_tonic(shifted), Some(9), "expected A after match, got {}", key_name(shifted));
}
