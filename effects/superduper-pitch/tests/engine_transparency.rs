//! How much does each engine dirty a signal, and does the answer depend on
//! the material rather than on the shift?
//!
//! This is the measurement that decides which engine a corrector should use,
//! and it caught a real defect: TD-PSOLA leaves broadband noise only a few dB
//! below the harmonics at UNITY shift on smooth material, because each grain
//! is read around a snapped glottal epoch but written to an unsnapped
//! synthesis mark — with no pulse to snap to, that read point wanders and the
//! grains overlap-add at scrambled phase. The phase vocoder is essentially
//! transparent on the same input.
//!
//! `engine_matrix` prints the whole picture: 4 sources × 2 engines × 3 shifts.
//! **Baseline recorded 2026-08-27** (noise-to-harmonic in dB, lower = cleaner;
//! `in` is the unprocessed source measured the same way):
//!
//! ```text
//! source                    in  engine    0 st   +25 c  +12 st
//! pulsed voice           -23.9  PSOLA    -24.2   -24.9   -21.9
//!                               pvoc     -23.9   -26.9   -19.8
//! smooth tone            -66.9  PSOLA     -2.6    -2.6     2.6
//!                               pvoc     -66.9   -54.5   -17.9
//! voiced (glottal)       -20.5  PSOLA    -20.8   -21.8   -19.0
//!                               pvoc     -20.2   -22.2   -15.8
//! breathy take            -9.1  PSOLA     -1.4    -1.4     2.6
//!                               pvoc      -9.0   -14.0    -7.6
//! ```
//!
//! Read it as three groups, and note the third is the one that changed the
//! plan:
//!
//! 1. **Pulsed** (impulse train, and the glottal voice at a normal open
//!    quotient): both engines stay within ~1 dB of the input at unity. PSOLA
//!    is fine and keeps its formant independence, so it wins.
//! 2. **Smooth** (sum-of-sines, a synth, a sustained kubyz): PSOLA turns
//!    −66.9 dB into −2.6 dB — 64 dB worse. pvoc is exact. Not a contest.
//! 3. **Breathy** — the surprise. Aspiration noise does not merely *add*
//!    noise, it *softens the epoch*: with a long open quotient there is no
//!    sharp closure to snap to, and PSOLA degrades the ratio by 7.7 dB
//!    (−9.1 → −1.4) where pvoc holds it (−9.0). So the router cannot key on
//!    "is it a voice" — a breathy voice needs pvoc as much as a synth does.
//!    That is why the descriptor in task 1.2 measures the epoch, not
//!    voicedness, and why the threshold in task 1.3 must sit above the
//!    breathy source's value, not just above the smooth tone's.
//!
//! The two synthetic voices are deterministic so CI numbers are stable. Point
//! `SDSP_VOCAL_TAKE` / `SDSP_BREATHY_TAKE` at a WAV to run the same matrix on
//! a real recording. **Caveat:** this metric assumes ONE stationary f0 — on a
//! sung phrase the pitch moves, every harmonic lands off the fixed grid, and
//! the ratio goes positive and stops meaning anything (a real lead-vocal take
//! measured +22.3 dB at the input). Rows like that are flagged in the output.
//! Use a file source to confirm an engine does not blow up on real material,
//! not to read absolute numbers off it.

use superduper_pitch::pvoc::PhaseVocoder;
use superduper_synth_core::psola::{PitchParams, PitchShifter};

const SR: f32 = 48_000.0;
const F0: f32 = 220.0;
const BLOCK: usize = 256;

/// Smooth harmonic tone — a synth, a sustained kubyz, any sum-of-sines. No
/// glottal pulse anywhere in it.
fn tone(secs: f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            (1..=6)
                .map(|k| (core::f32::consts::TAU * F0 * k as f32 * t).sin() / k as f32)
                .sum::<f32>()
                * 0.3
        })
        .collect()
}

/// Impulse train through two formant resonators — the textbook signal PSOLA
/// is designed for: one sharp, unambiguous epoch per period.
fn pulsed(secs: f32) -> Vec<f32> {
    use superduper_synth_core::dsp_blocks::Biquad;
    let n = (secs * SR) as usize;
    let period = SR / F0;
    let mut b1 = Biquad::default();
    b1.set_bandpass(SR, 700.0, 5.0);
    let mut b2 = Biquad::default();
    b2.set_bandpass(SR, 1800.0, 8.0);
    let mut ph = 0.0f32;
    (0..n)
        .map(|_| {
            ph += 1.0;
            let src = if ph >= period {
                ph -= period;
                1.0
            } else {
                0.0
            };
            (b1.process(src) + 0.5 * b2.process(src)) * 0.6
        })
        .collect()
}

/// Rosenberg glottal-flow pulses with jitter + shimmer through three
/// formants, plus `breath` of aspiration noise. `breath = 0.02` is a normal
/// sung note; `breath = 0.5` with a soft pulse is a breathy/whispered take —
/// the case where the epoch is real but buried.
fn voiced(secs: f32, breath: f32, open: f32) -> Vec<f32> {
    use superduper_synth_core::dsp_blocks::{Biquad, Xorshift};
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
    // Aspiration is band-limited, not flat white — real breath noise sits
    // above ~1 kHz where the vocal tract is not damped by the glottis.
    let mut air = Biquad::default();
    air.set_bandpass(SR, 2600.0, 0.8);

    let mut ph = 0.0f32;
    let mut period = SR / F0;
    let mut amp = 1.0f32;
    let mut prev_g = 0.0f32;
    (0..n)
        .map(|_| {
            ph += 1.0;
            if ph >= period {
                ph -= period;
                // jitter ±0.4 %, shimmer ±6 % — enough to be a voice, not
                // enough to blur the harmonic grid the metric measures on.
                period = SR / F0 * (1.0 + 0.004 * rng.next_bipolar());
                amp = 1.0 + 0.06 * rng.next_bipolar();
            }
            // Rosenberg pulse: raised-cosine open phase, cosine-quarter close.
            let t = ph / period;
            let (t1, t2) = (open, open * 0.4);
            let g = amp
                * if t < t1 {
                    0.5 * (1.0 - (core::f32::consts::PI * t / t1).cos())
                } else if t < t1 + t2 {
                    (core::f32::consts::FRAC_PI_2 * (t - t1) / t2).cos()
                } else {
                    0.0
                };
            // The excitation is the DERIVATIVE of glottal flow — the sharp
            // negative spike at closure IS the epoch PSOLA looks for. A
            // longer open phase (`open`) softens it, which is exactly what
            // makes a breathy take hard to snap to.
            let exc = (g - prev_g) * 40.0;
            prev_g = g;
            let noise = air.process(rng.next_bipolar()) * breath;
            let out: f32 = bank.iter_mut().map(|b| b.process(exc + noise)).sum();
            out * 0.5
        })
        .collect()
}

/// Optional real recording, mono-summed and naively resampled to `SR`.
/// Missing env var or unreadable file → `None`, so CI stays hermetic.
fn take_from_env(var: &str) -> Option<Vec<f32>> {
    let path = std::env::var(var).ok()?;
    let w = superduper_synth_core::wav::parse_wav_file(std::path::Path::new(&path)).ok()?;
    let ratio = w.sample_rate as f32 / SR;
    let frames = w.frame_count();
    let n = (frames as f32 / ratio) as usize;
    // Take the loudest 2 s so a leading silence doesn't dominate the window.
    let want = (2.0 * SR) as usize;
    let all: Vec<f32> = (0..n)
        .map(|i| w.read_mono_at(((i as f32 * ratio) as usize).min(frames - 1)))
        .collect();
    if all.len() <= want {
        return Some(all);
    }
    let step = (0.05 * SR) as usize;
    let best = (0..=(all.len() - want))
        .step_by(step)
        .max_by(|&a, &b| {
            let e = |s: usize| all[s..s + want].iter().map(|v| v * v).sum::<f32>();
            e(a).total_cmp(&e(b))
        })
        .unwrap_or(0);
    Some(all[best..best + want].to_vec())
}

struct Source {
    name: &'static str,
    x: Vec<f32>,
    /// Fundamental the harmonic grid is measured against.
    f0: f32,
}

fn sources() -> Vec<Source> {
    let mut v = vec![
        Source { name: "pulsed voice", x: pulsed(2.0), f0: F0 },
        Source { name: "smooth tone", x: tone(2.0), f0: F0 },
    ];
    match take_from_env("SDSP_VOCAL_TAKE") {
        Some(x) => {
            let f0 = measured_f0(&x);
            v.push(Source { name: "vocal take (file)", x, f0 });
        }
        None => v.push(Source { name: "voiced (glottal)", x: voiced(2.0, 0.02, 0.4), f0: F0 }),
    }
    match take_from_env("SDSP_BREATHY_TAKE") {
        Some(x) => {
            let f0 = measured_f0(&x);
            v.push(Source { name: "breathy take (file)", x, f0 });
        }
        None => v.push(Source { name: "breathy take", x: voiced(2.0, 0.5, 0.62), f0: F0 }),
    }
    v
}

/// A real take is never at exactly 220 Hz, and the metric needs the grid it
/// actually sits on or every harmonic reads as noise.
fn measured_f0(x: &[f32]) -> f32 {
    superduper_synth_core::pitch::detect_pitch_hz(x, SR as u32).unwrap_or(F0)
}

/// Noise energy (everything off the harmonic grid) relative to harmonic
/// energy, in dB. Lower is cleaner.
fn noise_to_harmonic(x: &[f32], f0: f32) -> f32 {
    let start = (0.6 * SR) as usize;
    let len = (1.0 * SR) as usize;
    let seg: Vec<f32> = x[start..start + len]
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let w = 0.5 - 0.5 * (core::f32::consts::TAU * i as f32 / len as f32).cos();
            v * w
        })
        .collect();
    let spec = superduper_synth_core::analysis::magnitude_spectrum_db(&seg);
    let bin_hz = SR / len as f32;
    let (mut harm, mut noise) = (0.0f64, 0.0f64);
    for (i, &db) in spec.iter().enumerate() {
        let hz = i as f32 * bin_hz;
        if hz < 60.0 || hz > 8000.0 {
            continue;
        }
        let lin = 10f64.powf(db as f64 / 20.0);
        let e = lin * lin;
        let near = (1..=36).any(|k| (hz - f0 * k as f32).abs() < 7.0 * bin_hz);
        if near {
            harm += e;
        } else {
            noise += e;
        }
    }
    10.0 * ((noise / harm.max(1e-30)) as f32).log10()
}

fn run<F: FnMut(&[f32], &mut [f32])>(input: &[f32], mut f: F) -> Vec<f32> {
    let mut out = vec![0.0; input.len()];
    let mut at = 0;
    while at < input.len() {
        let n = BLOCK.min(input.len() - at);
        let (i, o) = (&input[at..at + n], &mut out[at..at + n]);
        f(i, o);
        at += n;
    }
    out
}

fn params(st: f32) -> PitchParams {
    PitchParams { pitch_st: st, formant_st: 0.0, mix: 1.0, output_lin: 1.0, bypassed: false }
}

/// The 95 Hz default floor never locks on a low male voice, so derive it from
/// the material — otherwise the matrix would measure the floor, not the
/// engine (this is the same derivation task 2.3 puts inside `PitchEngine`).
fn psola_for(f0: f32) -> PitchShifter {
    let mut s = PitchShifter::with_range(SR, BLOCK, 0, 95.0f32.min(f0 * 0.8), 1000.0);
    s.prime(1.0, 1.0);
    s
}

fn psola_out(x: &[f32], f0: f32, st: f32) -> Vec<f32> {
    let mut s = psola_for(f0);
    let p = params(st);
    let mut r = vec![0.0; BLOCK];
    run(x, |i, o| {
        r.resize(i.len(), 0.0);
        s.process(i, i, o, &mut r, &p);
    })
}

fn pvoc_out(x: &[f32], st: f32) -> Vec<f32> {
    let mut v = PhaseVocoder::new(SR, 0);
    let p = params(st);
    let mut r = vec![0.0; BLOCK];
    run(x, |i, o| {
        r.resize(i.len(), 0.0);
        v.process(i, i, o, &mut r, &p);
    })
}

/// The baseline table in this file's doc comment. Prints, asserts only the
/// one relationship the whole track hangs on: on smooth material PSOLA is
/// far worse than the phase vocoder, and on pulsed material it is not.
#[test]
fn engine_matrix() {
    const SHIFTS: [(&str, f32); 3] = [("0 st", 0.0), ("+25 c", 0.25), ("+12 st", 12.0)];
    println!("\n{:<20} {:>7}  {:<6} {:>7} {:>7} {:>7}", "source", "in", "engine", "0 st", "+25 c", "+12 st");
    let mut smooth_psola = 0.0f32;
    let mut smooth_pvoc = 0.0f32;
    let mut pulsed_psola = 0.0f32;
    for s in sources() {
        let nin = noise_to_harmonic(&s.x, s.f0);
        for engine in ["PSOLA", "pvoc"] {
            let mut row = [0.0f32; 3];
            for (k, (_, st)) in SHIFTS.iter().enumerate() {
                let y = match engine {
                    "PSOLA" => psola_out(&s.x, s.f0, *st),
                    _ => pvoc_out(&s.x, *st),
                };
                // The output grid moves with the shift.
                row[k] = noise_to_harmonic(&y, s.f0 * (st / 12.0).exp2());
            }
            if s.name == "smooth tone" {
                if engine == "PSOLA" {
                    smooth_psola = row[0];
                } else {
                    smooth_pvoc = row[0];
                }
            }
            if s.name == "pulsed voice" && engine == "PSOLA" {
                pulsed_psola = row[0];
            }
            let label = if engine == "PSOLA" { s.name } else { "" };
            let inn = if engine == "PSOLA" { format!("{nin:>7.1}") } else { " ".repeat(7) };
            // A positive input ratio means the harmonics are not on the fixed
            // grid this metric measures — a moving pitch. Say so rather than
            // let a reader take the number at face value.
            let flag = if engine == "PSOLA" && nin > 0.0 { "  <- pitch moves, ratio not meaningful" } else { "" };
            println!(
                "{label:<20} {inn}  {engine:<6} {:>7.1} {:>7.1} {:>7.1}{flag}",
                row[0], row[1], row[2]
            );
        }
    }
    assert!(
        smooth_psola - smooth_pvoc > 40.0,
        "the whole track assumes PSOLA is far worse than pvoc on smooth material; \
         got PSOLA {smooth_psola:.1} dB vs pvoc {smooth_pvoc:.1} dB"
    );
    assert!(
        pulsed_psola < -20.0,
        "PSOLA should stay clean on pulsed material, got {pulsed_psola:.1} dB"
    );
}

#[test]
fn phase_vocoder_is_transparent_at_unity() {
    let x = tone(2.0);
    let y = pvoc_out(&x, 0.0);
    let (nin, nout) = (noise_to_harmonic(&x, F0), noise_to_harmonic(&y, F0));
    println!("phase vocoder @ unity: in {nin:.1} dB → out {nout:.1} dB");
    assert!(nout < -60.0, "phase vocoder should stay clean, got {nout:.1} dB");
}

#[test]
fn psola_unity_shift_noise_is_a_known_defect() {
    let x = tone(2.0);
    let nout = noise_to_harmonic(&psola_out(&x, F0, 0.0), F0);
    println!(
        "PSOLA @ unity: {nout:.1} dB  (known defect — epoch snap moves the read \
         point only; see synth-core/CLAUDE.md)"
    );
    assert!(nout < 0.0, "PSOLA got even worse than the recorded defect: {nout:.1} dB");
}

/// The claim that turns "rewrite the engine" into "route around it": the same
/// PSOLA call, at the same unity shift, is clean on a pulsed source and
/// destructive on a smooth one. If this ever stops holding, the routing in
/// `PitchEngine` is solving the wrong problem.
#[test]
fn psola_transparency_depends_on_epoch_sharpness() {
    let p = pulsed(2.0);
    let s = tone(2.0);
    let (pin, pout) = (noise_to_harmonic(&p, F0), noise_to_harmonic(&psola_out(&p, F0, 0.0), F0));
    let (sin_, sout) = (noise_to_harmonic(&s, F0), noise_to_harmonic(&psola_out(&s, F0, 0.0), F0));
    println!("PSOLA @ unity, PULSED source: in {pin:.1} dB → out {pout:.1} dB");
    println!("PSOLA @ unity, SMOOTH source: in {sin_:.1} dB → out {sout:.1} dB");
    assert!(
        (pout - pin).abs() < 3.0,
        "PSOLA should be near-transparent on pulsed material: {pin:.1} → {pout:.1} dB"
    );
    assert!(
        sout - pout > 15.0,
        "the scope defect is the premise of this track: smooth {sout:.1} dB vs pulsed {pout:.1} dB"
    );
}
