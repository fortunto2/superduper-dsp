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

use sdsp_test_kit::signals::{
    breathy, noise_to_harmonic, pulsed, smooth, take_from_env, voiced, F0, SR,
};
use superduper_synth_core::psola::{PitchParams, PitchShifter};
use superduper_synth_core::pvoc::PhaseVocoder;

const BLOCK: usize = 256;

struct Source {
    name: &'static str,
    x: Vec<f32>,
    /// Fundamental the harmonic grid is measured against.
    f0: f32,
}

fn sources() -> Vec<Source> {
    let mut v = vec![
        Source { name: "pulsed voice", x: pulsed(2.0), f0: F0 },
        Source { name: "smooth tone", x: smooth(2.0), f0: F0 },
    ];
    match take_from_env("SDSP_VOCAL_TAKE", 2.0) {
        Some(x) => {
            let f0 = measured_f0(&x);
            v.push(Source { name: "vocal take (file)", x, f0 });
        }
        None => v.push(Source { name: "voiced (glottal)", x: voiced(2.0), f0: F0 }),
    }
    match take_from_env("SDSP_BREATHY_TAKE", 2.0) {
        Some(x) => {
            let f0 = measured_f0(&x);
            v.push(Source { name: "breathy take (file)", x, f0 });
        }
        None => v.push(Source { name: "breathy take", x: breathy(2.0), f0: F0 }),
    }
    v
}

/// A real take is never at exactly 220 Hz, and the metric needs the grid it
/// actually sits on or every harmonic reads as noise.
fn measured_f0(x: &[f32]) -> f32 {
    superduper_synth_core::pitch::detect_pitch_hz(x, SR as u32).unwrap_or(F0)
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
        let nin = noise_to_harmonic(&s.x, s.f0, 0.6);
        for engine in ["PSOLA", "pvoc"] {
            let mut row = [0.0f32; 3];
            for (k, (_, st)) in SHIFTS.iter().enumerate() {
                let y = match engine {
                    "PSOLA" => psola_out(&s.x, s.f0, *st),
                    _ => pvoc_out(&s.x, *st),
                };
                // The output grid moves with the shift.
                row[k] = noise_to_harmonic(&y, s.f0 * (st / 12.0).exp2(), 0.6);
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
    let x = smooth(2.0);
    let y = pvoc_out(&x, 0.0);
    let (nin, nout) = (noise_to_harmonic(&x, F0, 0.6), noise_to_harmonic(&y, F0, 0.6));
    println!("phase vocoder @ unity: in {nin:.1} dB → out {nout:.1} dB");
    assert!(nout < -60.0, "phase vocoder should stay clean, got {nout:.1} dB");
}

/// Where PSOLA alone lands on a smooth tone at unity shift, as measured
/// 2026-08-27. It is a record of a defect, not a target — `PitchEngine` routes
/// around it, and if the grain scheduler is ever fixed this number should
/// collapse and the bound below should be tightened rather than kept.
const PSOLA_SMOOTH_DEFECT_DB: f32 = -2.6;

/// Guards the defect against getting WORSE, and says so. A bound of the form
/// "must stay bad" would fail the day someone fixes the scheduler, which is
/// the opposite of what a regression test is for.
#[test]
fn psola_unity_shift_noise_is_a_known_defect() {
    let x = smooth(2.0);
    let nout = noise_to_harmonic(&psola_out(&x, F0, 0.0), F0, 0.6);
    println!(
        "PSOLA @ unity: {nout:.1} dB against a recorded {PSOLA_SMOOTH_DEFECT_DB:.1} dB \
         (epoch snap moves the read point only; see lesson 24)"
    );
    assert!(
        nout < PSOLA_SMOOTH_DEFECT_DB + 2.0,
        "PSOLA got worse than the recorded defect: {nout:.1} dB vs {PSOLA_SMOOTH_DEFECT_DB:.1}"
    );
    if nout < PSOLA_SMOOTH_DEFECT_DB - 6.0 {
        println!(
            "NOTE: PSOLA is now {:.1} dB better than recorded — if that is a real fix, \
             re-record PSOLA_SMOOTH_DEFECT_DB and tighten this bound.",
            PSOLA_SMOOTH_DEFECT_DB - nout
        );
    }
}

/// The claim that turns "rewrite the engine" into "route around it": the same
/// PSOLA call, at the same unity shift, is clean on a pulsed source and
/// destructive on a smooth one. If this ever stops holding, the routing in
/// `PitchEngine` is solving the wrong problem.
#[test]
fn psola_transparency_depends_on_epoch_sharpness() {
    let p = pulsed(2.0);
    let s = smooth(2.0);
    let (pin, pout) = (noise_to_harmonic(&p, F0, 0.6), noise_to_harmonic(&psola_out(&p, F0, 0.0), F0, 0.6));
    let (sin_, sout) = (noise_to_harmonic(&s, F0, 0.6), noise_to_harmonic(&psola_out(&s, F0, 0.0), F0, 0.6));
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
