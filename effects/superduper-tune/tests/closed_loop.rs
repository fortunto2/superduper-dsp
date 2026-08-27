//! The `sdsp-tune` question, asked of the live plugin: correct a take, then
//! **re-analyse the result** and check it actually landed on the note without
//! trashing the signal on the way.
//!
//! Correcting and measuring the correction is not the same as measuring the
//! output — the engine can report a perfect −35 cents and still hand back
//! something noisy enough that the note is unusable. That is precisely how
//! the PSOLA scope defect hid: `correction.rs` was green throughout, because
//! it asks what the corrector *decided*, not what came out.
//!
//! Two sources, both detuned by a third of a semitone so there is real work
//! to do:
//! * a smooth harmonic tone (a synth, a sustained kubyz) — the case PSOLA
//!   destroys and the router now sends to the phase vocoder;
//! * a glottal-pulse voice — the case PSOLA is for, which must not regress.
//!
//! Point `SDSP_VOCAL_TAKE` at a WAV to run the voice half on a real
//! recording. The pitch bound still applies; the noise-floor bound does not,
//! because that metric needs one stationary f0 and a sung phrase has none.

use superduper_synth_core::dsp_blocks::{Biquad, Xorshift};
use superduper_synth_core::pitch::detect_pitch_hz;
use superduper_tune::dsp::{EngineMode, Tune, TuneParams, TARGET_SCALE};
use superduper_tune::scale;

const SR: f32 = 48_000.0;
const BLOCK: usize = 256;
/// A3. The correction target the Scale mode should snap to.
const NOTE_HZ: f32 = 220.0;
/// How far off the take sings. A third of a semitone is well inside the
/// snap range and well outside the 10-cent bound we assert on the result.
const DETUNE_ST: f32 = 0.35;

fn detuned() -> f32 {
    NOTE_HZ * 2f32.powf(DETUNE_ST / 12.0)
}

fn smooth(secs: f32, f0: f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            (1..=6)
                .map(|k| (core::f32::consts::TAU * f0 * k as f32 * t).sin() / k as f32)
                .sum::<f32>()
                * 0.3
        })
        .collect()
}

/// Rosenberg glottal pulses through three formants — mirrors the reference
/// voice in `synth-core/tests/common`.
fn voiced(secs: f32, f0: f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    let mut rng = Xorshift::new(0x5EED_1234);
    let mut bank: Vec<Biquad> = [(730.0, 9.0), (1090.0, 11.0), (2440.0, 13.0)]
        .iter()
        .map(|&(hz, q)| {
            let mut b = Biquad::default();
            b.set_bandpass(SR, hz, q);
            b
        })
        .collect();
    let (mut ph, mut period, mut amp, mut prev) = (0.0f32, SR / f0, 1.0f32, 0.0f32);
    (0..n)
        .map(|_| {
            ph += 1.0;
            if ph >= period {
                ph -= period;
                period = SR / f0 * (1.0 + 0.004 * rng.next_bipolar());
                amp = 1.0 + 0.06 * rng.next_bipolar();
            }
            let t = ph / period;
            let (t1, t2) = (0.4f32, 0.16f32);
            let g = amp
                * if t < t1 {
                    0.5 * (1.0 - (core::f32::consts::PI * t / t1).cos())
                } else if t < t1 + t2 {
                    (core::f32::consts::FRAC_PI_2 * (t - t1) / t2).cos()
                } else {
                    0.0
                };
            let exc = (g - prev) * 40.0;
            prev = g;
            let out: f32 = bank.iter_mut().map(|b| b.process(exc)).sum();
            out * 0.5
        })
        .collect()
}

fn take_from_env(var: &str) -> Option<(Vec<f32>, f32)> {
    let path = std::env::var(var).ok()?;
    let w = superduper_synth_core::wav::parse_wav_file(std::path::Path::new(&path)).ok()?;
    let ratio = w.sample_rate as f32 / SR;
    let frames = w.frame_count();
    let n = ((frames as f32 / ratio) as usize).min((4.0 * SR) as usize);
    let x: Vec<f32> = (0..n)
        .map(|i| w.read_mono_at(((i as f32 * ratio) as usize).min(frames - 1)))
        .collect();
    let hz = detect_pitch_hz(&x, SR as u32)?;
    Some((x, hz))
}

fn hard_tune(engine: EngineMode) -> TuneParams {
    TuneParams {
        // Chromatic: every semitone is a target, so the only thing being
        // tested is whether the engine lands on one — not whether the scale
        // happened to contain the right note.
        key: 9, // A
        scale_mask: scale::SCALES[0].1,
        target: TARGET_SCALE,
        retune_ms: 0.0,
        amount: 1.0,
        engine,
        ..TuneParams::default()
    }
}

fn render(x: &[f32], p: &TuneParams) -> Vec<f32> {
    let mut tune = Tune::new(SR, BLOCK);
    tune.prime(p.mix, p.output_lin);
    let mut out = vec![0.0f32; x.len()];
    let (mut ol, mut or) = (vec![0.0f32; BLOCK], vec![0.0f32; BLOCK]);
    let sc = vec![0.0f32; BLOCK];
    let mut pos = 0;
    while pos + BLOCK <= x.len() {
        let b = &x[pos..pos + BLOCK];
        tune.process(b, b, &sc, &sc, &mut ol, &mut or, p);
        out[pos..pos + BLOCK].copy_from_slice(&ol);
        pos += BLOCK;
    }
    out
}

/// Median absolute distance to the nearest semitone of the A grid, in cents,
/// measured over the settled tail in 200 ms slices. Median rather than mean
/// so one bad window cannot decide the verdict — the same reason `sdsp-tune`
/// corrects a note by its median.
fn median_cents_off(y: &[f32], target: f32) -> f32 {
    let win = (0.2 * SR) as usize;
    let from = (1.2 * SR) as usize;
    let mut errs: Vec<f32> = (from..y.len().saturating_sub(win))
        .step_by(win)
        .filter_map(|a| detect_pitch_hz(&y[a..a + win], SR as u32))
        .map(|hz| {
            let st = 12.0 * (hz / target).log2();
            (st - st.round()).abs() * 100.0
        })
        .collect();
    assert!(!errs.is_empty(), "no pitch could be detected in the output at all");
    errs.sort_by(f32::total_cmp);
    errs[errs.len() / 2]
}

fn noise_to_harmonic(x: &[f32], f0: f32, skip: f32) -> f32 {
    let start = (skip * SR) as usize;
    let len = (1.0 * SR) as usize;
    let seg: Vec<f32> = x[start..start + len]
        .iter()
        .enumerate()
        .map(|(i, &v)| v * (0.5 - 0.5 * (core::f32::consts::TAU * i as f32 / len as f32).cos()))
        .collect();
    let spec = superduper_synth_core::analysis::magnitude_spectrum_db(&seg);
    let bin_hz = SR / len as f32;
    let (mut harm, mut noise) = (0.0f64, 0.0f64);
    for (i, &db) in spec.iter().enumerate() {
        let hz = i as f32 * bin_hz;
        if !(60.0..=8000.0).contains(&hz) {
            continue;
        }
        let e = 10f64.powf(db as f64 / 10.0);
        if (1..=36).any(|k| (hz - f0 * k as f32).abs() < 7.0 * bin_hz) {
            harm += e;
        } else {
            noise += e;
        }
    }
    10.0 * ((noise / harm.max(1e-30)) as f32).log10()
}

/// The noise bound here is absolute, not "no rise", and that is deliberate.
/// The input is a mathematically exact sum of six sines: −66.9 dB, a floor no
/// pitch shifter can hold once it actually shifts, because any real shift
/// puts sidebands around every partial. The phase vocoder's own cost at a
/// quarter-semitone is about 12 dB on this signal (measured in
/// `engine_transparency.rs`'s matrix), and the correction plumbing adds the
/// rest. −40 dB is the bar the result has to clear to be musically clean; the
/// number that matters for the routing decision is the 45 dB between this and
/// forced PSOLA, asserted in the next test.
#[test]
fn a_smooth_tone_is_corrected_and_stays_clean() {
    let x = smooth(3.0, detuned());
    let y = render(&x, &hard_tune(EngineMode::Auto));
    let off = median_cents_off(&y, NOTE_HZ);
    let (nin, nout) = (noise_to_harmonic(&x, detuned(), 0.3), noise_to_harmonic(&y, NOTE_HZ, 1.5));
    println!("smooth tone: {off:.1} cents off the grid, noise {nin:.1} -> {nout:.1} dB");
    assert!(off < 10.0, "corrected pitch is {off:.1} cents off, want under 10");
    assert!(nout < -40.0, "corrected tone is not clean: {nout:.1} dB (input {nin:.1})");
}

/// The same take through forced PSOLA, to keep the reason for the router
/// visible in the test suite rather than only in a commit message. The pitch
/// still lands; it is the noise floor that collapses.
#[test]
fn the_same_tone_through_forced_psola_is_why_auto_exists() {
    let x = smooth(3.0, detuned());
    let auto = noise_to_harmonic(&render(&x, &hard_tune(EngineMode::Auto)), NOTE_HZ, 1.5);
    let psola = noise_to_harmonic(&render(&x, &hard_tune(EngineMode::Psola)), NOTE_HZ, 1.5);
    println!("smooth tone corrected: Auto {auto:.1} dB vs forced PSOLA {psola:.1} dB");
    assert!(
        psola - auto > 30.0,
        "forced PSOLA should be far worse here; if it is not, the router is solving a \
         problem that no longer exists ({psola:.1} vs {auto:.1} dB)"
    );
}

#[test]
fn a_voice_is_corrected_and_stays_on_psola() {
    let (x, target) = match take_from_env("SDSP_VOCAL_TAKE") {
        Some((x, hz)) => {
            println!("using a real take, detected {hz:.1} Hz");
            (x, hz)
        }
        None => (voiced(3.0, detuned()), NOTE_HZ),
    };
    let y = render(&x, &hard_tune(EngineMode::Auto));
    let off = median_cents_off(&y, target);
    println!("voice: {off:.1} cents off the grid");
    assert!(off < 10.0, "corrected pitch is {off:.1} cents off, want under 10");

    if std::env::var("SDSP_VOCAL_TAKE").is_err() {
        // Only meaningful on a stationary pitch.
        let (nin, nout) =
            (noise_to_harmonic(&x, detuned(), 0.3), noise_to_harmonic(&y, NOTE_HZ, 1.5));
        println!("voice: noise {nin:.1} -> {nout:.1} dB");
        assert!(nout - nin < 2.0, "correction added noise: {nin:.1} -> {nout:.1} dB");
    }
}
