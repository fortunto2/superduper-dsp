//! Phase-locking + formant-envelope quality metrics for Track mode.
//!
//! Written as the before/after instrument for the Laroche-Dolson phase
//! locking work (CLAUDE.md item 5). Baselines measured on the pre-locking
//! code are recorded next to each assert.
//!
//! Run with `-- --nocapture` to see the numbers.

use superduper_synth_core::analysis::spectrum_with_freq;
use superduper_synth_core::psola::PitchParams;
use superduper_synth_core::pvoc::PhaseVocoder;

const SR: f32 = 48_000.0;

fn run_track(m: &[f32], pitch: f32, formant: f32) -> Vec<f32> {
    let n = m.len();
    let mut out = vec![0.0f32; n];
    let mut pv = PhaseVocoder::new(SR, superduper_synth_core::pvoc::LATENCY);
    let p = PitchParams { pitch_st: pitch, formant_st: formant, mix: 1.0, output_lin: 1.0, bypassed: false };
    let mut i = 0;
    while i < n {
        let end = (i + 512).min(n);
        let inb = &m[i..end];
        let mut ol = vec![0.0f32; end - i];
        let mut or = vec![0.0f32; end - i];
        pv.process(inb, inb, &mut ol, &mut or, &p);
        out[i..end].copy_from_slice(&ol);
        i = end;
    }
    out
}

fn settled_window(x: &[f32]) -> &[f32] {
    let start = (SR * 1.0) as usize;
    &x[start..start + 32768]
}

/// Signal-to-junk: energy within ±3 % of each expected partial vs everything
/// else above 60 Hz. Linear power ratio in dB.
fn signal_to_junk_db(x: &[f32], expected_hz: &[f32]) -> f32 {
    let spec = spectrum_with_freq(x, SR);
    let mut sig = 0.0f64;
    let mut junk = 0.0f64;
    for &(f, db) in &spec {
        if f < 60.0 || f > 18_000.0 {
            continue;
        }
        let p = 10f64.powf(db as f64 / 10.0);
        if expected_hz.iter().any(|&e| (f - e).abs() < e * 0.03) {
            sig += p;
        } else {
            junk += p;
        }
    }
    (10.0 * (sig / junk.max(1e-30)).log10()) as f32
}

/// Rosenberg-ish voiced source through three formant resonators.
fn synth_vowel(f0: f32, formants: &[(f32, f32)], n: usize) -> Vec<f32> {
    let mut pulse = vec![0.0f32; n];
    let period = (SR / f0) as usize;
    for i in (0..n).step_by(period) {
        pulse[i] = 1.0;
    }
    // Three parallel 2-pole resonators.
    let mut out = vec![0.0f32; n];
    for &(fc, bw) in formants {
        let r = (-std::f32::consts::PI * bw / SR).exp();
        let cosw = (std::f32::consts::TAU * fc / SR).cos();
        let a1 = -2.0 * r * cosw;
        let a2 = r * r;
        let g = (1.0 - r) * (1.0 - r);
        let (mut z1, mut z2) = (0.0f32, 0.0f32);
        for i in 0..n {
            let y = g * pulse[i] - a1 * z1 - a2 * z2;
            z2 = z1;
            z1 = y;
            out[i] += y;
        }
    }
    out
}

/// Spectral centroid (Hz) of the 300..4000 Hz band — tracks formant placement.
fn centroid_hz(x: &[f32]) -> f32 {
    let spec = spectrum_with_freq(x, SR);
    let mut num = 0.0f64;
    let mut den = 0.0f64;
    for &(f, db) in &spec {
        if !(300.0..=4000.0).contains(&f) {
            continue;
        }
        let p = 10f64.powf(db as f64 / 10.0);
        num += f as f64 * p;
        den += p;
    }
    (num / den.max(1e-30)) as f32
}

#[test]
fn single_tone_sidebands() {
    let n = (SR * 3.0) as usize;
    let m: Vec<f32> = (0..n)
        .map(|i| 0.5 * (std::f32::consts::TAU * 330.0 * i as f32 / SR).sin())
        .collect();
    let alpha = 2f32.powf(3.0 / 12.0);
    let out = run_track(&m, 3.0, 0.0);
    let sj = signal_to_junk_db(settled_window(&out), &[330.0 * alpha]);
    eprintln!("single tone +3st: signal-to-junk {sj:.1} dB");
    // Pre-locking baseline 36.8 dB; identity phase locking measured 50.7.
    assert!(sj > 45.0, "phase locking regressed: {sj:.1} dB (was 50.7)");
}

#[test]
fn triad_concentration() {
    use std::f32::consts::TAU;
    let tones = [261.63f32, 329.63, 392.00];
    let n = (SR * 3.0) as usize;
    let m: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            tones.iter().map(|&f| (TAU * f * t).sin()).sum::<f32>() / 3.0 * 0.5
        })
        .collect();
    let ratio = 2f32.powf(2.0 / 12.0);
    let expected: Vec<f32> = tones.iter().map(|&f| f * ratio).collect();
    let out = run_track(&m, 2.0, 0.0);
    let sj = signal_to_junk_db(settled_window(&out), &expected);
    eprintln!("triad +2st: signal-to-junk {sj:.1} dB");
    // Pre-locking baseline 46.9 dB; with locking 49.1.
    assert!(sj > 45.0, "chord concentration regressed: {sj:.1} dB (was 49.1)");
}

#[test]
fn formant_counter_shift_holds_the_envelope() {
    let formants = [(700.0, 110.0), (1200.0, 120.0), (2600.0, 160.0)];
    let n = (SR * 3.0) as usize;
    let m = synth_vowel(110.0, &formants, n);
    let c_in = centroid_hz(settled_window(&m));
    // +5 st pitch with −5 st formant = "shift the pitch, keep the timbre".
    let held = run_track(&m, 5.0, -5.0);
    let naive = run_track(&m, 5.0, 0.0);
    let c_held = centroid_hz(settled_window(&held));
    let c_naive = centroid_hz(settled_window(&naive));
    eprintln!(
        "vowel centroid: in {c_in:.0} Hz, +5st naive {c_naive:.0} Hz, +5st/-5st held {c_held:.0} Hz"
    );
    let err_held = (c_held - c_in).abs();
    let err_naive = (c_naive - c_in).abs();
    // Boxcar-8 baseline err 133 Hz; boxcar+proportional cascade measured 41.
    assert!(
        err_held < err_naive * 0.5,
        "formant envelope regressed: held {err_held:.0} Hz vs naive {err_naive:.0} (was 41 vs 314)"
    );
}
