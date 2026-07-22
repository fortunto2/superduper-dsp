//! The headline test: does the shifter shift by the right amount, and is the
//! formant control independent of pitch?
//!
//! Run: `cargo test -p superduper-pitch --test pitch_accuracy -- --nocapture`

use superduper_pitch::dsp::{PitchParams, PitchShifter};
use superduper_synth_core::analysis::spectrum_with_freq;
use superduper_synth_core::dsp_blocks::Biquad;

const SR: f32 = 48_000.0;

/// Source-filter voice at `f0`: a glottal impulse train (sharp epochs, like
/// real voiced speech) through two formant resonators. The sharp pulses are
/// what pitch-synchronous PSOLA needs; two formants (700 / 1800 Hz) make a
/// formant shift show up as a moving spectral peak.
fn voice(f0: f32, n: usize) -> Vec<f32> {
    let period = SR / f0;
    let mut b1 = Biquad::default();
    b1.set_bandpass(SR, 700.0, 5.0);
    let mut b2 = Biquad::default();
    b2.set_bandpass(SR, 1800.0, 8.0);
    let mut ph = 0.0f32;
    let mut out = vec![0.0f32; n];
    for o in out.iter_mut() {
        ph += 1.0;
        let src = if ph >= period {
            ph -= period;
            1.0
        } else {
            0.0
        };
        *o = (b1.process(src) + 0.5 * b2.process(src)) * 0.6;
    }
    out
}

/// Spectral centroid (Hz) over 120..6000 Hz.
fn centroid(x: &[f32]) -> f32 {
    let spec = spectrum_with_freq(x, SR);
    let mut num = 0.0f32;
    let mut den = 0.0f32;
    for &(f, db) in &spec {
        if f < 120.0 || f > 6000.0 {
            continue;
        }
        let lin = 10f32.powf(db / 20.0);
        num += f * lin;
        den += lin;
    }
    if den > 0.0 {
        num / den
    } else {
        0.0
    }
}

/// Run a whole buffer through the shifter (512-sample blocks) → left output.
fn run(modulator: &[f32], pitch_st: f32, formant_st: f32) -> Vec<f32> {
    let n = modulator.len();
    let mut out = vec![0.0f32; n];
    let mut sh = PitchShifter::new(SR, 512);
    let p = PitchParams {
        pitch_st,
        formant_st,
        mix: 1.0,
        output_lin: 1.0,
        bypassed: false,
    };
    let mut i = 0;
    while i < n {
        let end = (i + 512).min(n);
        let inb = &modulator[i..end];
        let mut ol = vec![0.0f32; end - i];
        let mut orr = vec![0.0f32; end - i];
        sh.process(inb, inb, &mut ol, &mut orr, &p);
        out[i..end].copy_from_slice(&ol);
        i = end;
    }
    out
}

/// Fundamental of the settled tail via autocorrelation (global max over a
/// musical lag range). Robust on the formant-heavy impulse-train voice, where
/// the fundamental is weak in the spectrum and YIN octave-confuses.
fn tail_f0(out: &[f32]) -> Option<f32> {
    let start = out.len() / 2;
    let x = &out[start..];
    let min_lag = (SR / 500.0) as usize; // up to 500 Hz
    let max_lag = (SR / 60.0) as usize; // down to 60 Hz
    if x.len() < max_lag * 2 {
        return None;
    }
    let mut best = min_lag;
    let mut best_v = f32::NEG_INFINITY;
    for lag in min_lag..=max_lag {
        let mut s = 0.0f32;
        let n = x.len() - lag;
        for i in 0..n {
            s += x[i] * x[i + lag];
        }
        s /= n as f32;
        if s > best_v {
            best_v = s;
            best = lag;
        }
    }
    Some(SR / best as f32)
}

#[test]
fn shift_up_one_octave_doubles_pitch() {
    let f0 = 150.0;
    let m = voice(f0, (SR * 3.0) as usize);
    let out = run(&m, 12.0, 0.0);
    let detected = tail_f0(&out).expect("should detect a pitch in the shifted output");
    println!("shift +12: in {f0} Hz → out {detected:.1} Hz (want ~{})", f0 * 2.0);
    let ratio = detected / f0;
    assert!((ratio - 2.0).abs() < 0.06, "expected ~2× (got {ratio:.3}×, {detected:.1} Hz)");
}

#[test]
fn shift_down_one_octave_halves_pitch() {
    let f0 = 200.0;
    let m = voice(f0, (SR * 3.0) as usize);
    let out = run(&m, -12.0, 0.0);
    let detected = tail_f0(&out).expect("should detect a pitch in the shifted output");
    println!("shift -12: in {f0} Hz → out {detected:.1} Hz (want ~{})", f0 * 0.5);
    let ratio = detected / f0;
    assert!((ratio - 0.5).abs() < 0.03, "expected ~0.5× (got {ratio:.3}×, {detected:.1} Hz)");
}

#[test]
fn regression_octave_down_130hz() {
    // The exact reported regression: f0 = 130, −12 st → must be 65 Hz (×0.5),
    // no clicks (max |x[n+1]-x[n]| < 0.1). Was 130 Hz + maxdelta 0.89.
    let f0 = 130.0;
    let m = voice(f0, (SR * 3.0) as usize);
    let out = run(&m, -12.0, 0.0);
    let detected = tail_f0(&out).expect("pitch");
    let tail = &out[out.len() / 2..];
    let max_jump = tail.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
    println!("f0=130 −12: out {detected:.1} Hz (want 65), maxdelta {max_jump:.4}");
    assert!((detected / f0 - 0.5).abs() < 0.03, "expected ~65 Hz, got {detected:.1}");
    assert!(max_jump < 0.1, "clicks: maxdelta {max_jump:.4}");
}

#[test]
fn shift_down_two_octaves_quarters_pitch() {
    // −24 st (α=0.25) — the extreme downshift; must actually drop two octaves,
    // not reconstruct the source.
    let f0 = 240.0;
    let m = voice(f0, (SR * 3.0) as usize);
    let out = run(&m, -24.0, 0.0);
    let detected = tail_f0(&out).expect("should detect a pitch");
    println!("shift -24: in {f0} Hz → out {detected:.1} Hz (want ~{})", f0 * 0.25);
    let ratio = detected / f0;
    assert!((ratio - 0.25).abs() < 0.04, "expected ~0.25× (got {ratio:.3}×, {detected:.1} Hz)");
}

#[test]
fn unshifted_preserves_pitch() {
    let f0 = 160.0;
    let m = voice(f0, (SR * 2.0) as usize);
    let out = run(&m, 0.0, 0.0);
    let detected = tail_f0(&out).expect("should detect a pitch");
    println!("shift 0: in {f0} Hz → out {detected:.1} Hz");
    assert!((detected / f0 - 1.0).abs() < 0.03, "expected ~1× (got {:.3}×)", detected / f0);
}

#[test]
fn formant_is_independent_of_pitch() {
    // Pitch untouched, Formant up 7 st: the FUNDAMENTAL must not move, but the
    // spectral centroid (formant region) must rise clearly.
    let f0 = 160.0;
    let m = voice(f0, (SR * 2.0) as usize);

    let flat = run(&m, 0.0, 0.0);
    let shifted = run(&m, 0.0, 7.0);
    let start = flat.len() * 2 / 3;

    let f0_flat = tail_f0(&flat).expect("f0 flat");
    let f0_shift = tail_f0(&shifted).expect("f0 shifted");
    let c_flat = centroid(&flat[start..]);
    let c_shift = centroid(&shifted[start..]);

    println!("formant +7: f0 {f0_flat:.1} → {f0_shift:.1} Hz (want unchanged)");
    println!("            centroid {c_flat:.0} → {c_shift:.0} Hz (want higher)");

    assert!(
        (f0_shift / f0_flat - 1.0).abs() < 0.04,
        "formant shift moved the pitch ({f0_flat:.1} → {f0_shift:.1})"
    );
    assert!(
        c_shift > c_flat * 1.10,
        "formant shift didn't raise the centroid ({c_flat:.0} → {c_shift:.0})"
    );
}
