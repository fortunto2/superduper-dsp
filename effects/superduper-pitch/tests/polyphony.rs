//! The critical Track-mode test: does the phase vocoder transpose a
//! **polyphonic chord** (all notes shift by the right ratio)? That's what
//! "change the key of a whole track/mix" needs, and what monophonic PSOLA
//! can't do.
//!
//! Run: `cargo test -p superduper-pitch --test polyphony -- --nocapture`

use superduper_pitch::dsp::PitchParams;
use superduper_pitch::pvoc::PhaseVocoder;
use superduper_synth_core::analysis::spectrum_with_freq;

const SR: f32 = 48_000.0;

fn chord(freqs: &[f32], n: usize) -> Vec<f32> {
    use std::f32::consts::TAU;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            freqs.iter().map(|&f| (TAU * f * t).sin()).sum::<f32>() / freqs.len() as f32 * 0.5
        })
        .collect()
}

fn run_track(m: &[f32], pitch: f32, formant: f32) -> Vec<f32> {
    let n = m.len();
    let mut out = vec![0.0f32; n];
    let mut pv = PhaseVocoder::new(SR, superduper_pitch::pvoc::LATENCY);
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

/// Peak magnitude (dB) within ±3 % of `hz`.
fn mag_at(spec: &[(f32, f32)], hz: f32) -> f32 {
    spec.iter()
        .filter(|(f, _)| (*f - hz).abs() < hz * 0.03)
        .map(|&(_, d)| d)
        .fold(f32::NEG_INFINITY, f32::max)
}

#[test]
fn transposes_polyphonic_chord() {
    // C-major triad: C4, E4, G4.
    let tones = [261.63f32, 329.63, 392.00];
    let m = chord(&tones, (SR * 3.0) as usize);
    let ratio = 2f32.powf(2.0 / 12.0); // +2 semitones

    let out = run_track(&m, 2.0, 0.0);
    assert!(out.iter().all(|v| v.is_finite()), "Track output has NaN/Inf");

    // FFT a 16k window well into the settled tail.
    let start = out.len() - 16384 - 2000;
    let win: Vec<f32> = out[start..start + 16384].to_vec();
    let spec = spectrum_with_freq(&win, SR);

    println!("Track Pitch +2 (ratio {ratio:.4}) on a C-E-G triad:");
    for &f in &tones {
        let shifted = f * ratio;
        let m_shift = mag_at(&spec, shifted);
        let m_orig = mag_at(&spec, f);
        println!("  {f:6.1} Hz → {shifted:6.1} Hz : shifted {m_shift:6.1} dB | original {m_orig:6.1} dB");
        assert!(m_shift > -45.0, "shifted tone {shifted:.1} Hz too weak ({m_shift:.1} dB)");
        assert!(
            m_shift > m_orig + 6.0,
            "chord tone {f:.1} didn't move up (shifted {m_shift:.1} dB vs original {m_orig:.1} dB)"
        );
    }
}

#[test]
fn track_transparent_peak_is_bounded() {
    // Sharp transients + tone. At Pitch 0 (transparent) the output peak must not
    // overshoot the input peak (the 2.67× OLA-normalization bug → would clip
    // drums). Allow a small margin.
    use std::f32::consts::TAU;
    let n = (SR * 2.0) as usize;
    let mut m = vec![0.0f32; n];
    for i in 0..n {
        let t = i as f32 / SR;
        m[i] = 0.3 * (TAU * 220.0 * t).sin();
        if i % (SR as usize / 8) == 0 {
            m[i] += 0.9; // an impulse every 1/8 s (drum-like transient)
        }
    }
    let out = run_track(&m, 0.0, 0.0);
    assert!(out.iter().all(|v| v.is_finite()), "NaN/Inf");
    let in_peak = m.iter().map(|v| v.abs()).fold(0.0, f32::max);
    // Skip the STFT warm-up region.
    let out_peak = out[SR as usize / 2..].iter().map(|v| v.abs()).fold(0.0, f32::max);
    println!(
        "Track transparent: in_peak {in_peak:.3} → out_peak {out_peak:.3} (ratio {:.2}×)",
        out_peak / in_peak
    );
    assert!(
        out_peak <= in_peak * 1.15,
        "Track peak overshoot at identity: {out_peak:.3} vs {in_peak:.3} ({:.2}×)",
        out_peak / in_peak
    );
}

#[test]
fn track_mode_shifts_mono_octave() {
    // Track engine must also handle a plain tone: +12 → ×2.
    let f0 = 220.0f32;
    let m = chord(&[f0], (SR * 3.0) as usize);
    let out = run_track(&m, 12.0, 0.0);
    assert!(out.iter().all(|v| v.is_finite()));
    let start = out.len() - 16384 - 2000;
    let win: Vec<f32> = out[start..start + 16384].to_vec();
    let spec = spectrum_with_freq(&win, SR);
    let m_up = mag_at(&spec, f0 * 2.0);
    let m_orig = mag_at(&spec, f0);
    println!("Track +12 on {f0} Hz: {} Hz {m_up:.1} dB | {f0} Hz {m_orig:.1} dB", f0 * 2.0);
    assert!(m_up > m_orig + 6.0, "octave-up peak not dominant ({m_up:.1} vs {m_orig:.1})");
}
