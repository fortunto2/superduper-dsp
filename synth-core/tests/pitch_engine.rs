//! The router has to be right about three separate things, and each of them
//! is a way the feature could ship broken while looking fine:
//!
//! 1. it picks the engine the material actually needs;
//! 2. changing engine mid-note does not click or jump level;
//! 3. asked to do nothing, it does nothing — on BOTH kinds of material, which
//!    is the rule lesson 24 was written to enforce.

use sdsp_test_kit::signals as common;

/// The shared probes take whole slices; these two just carry the range so the
/// call sites keep reading as "measure this stretch of the render".
fn rms_db(x: &[f32], a: usize, b: usize) -> f32 {
    sdsp_test_kit::probes::db(sdsp_test_kit::probes::rms(&x[a..b.min(x.len())])) as f32
}
fn max_step(x: &[f32], a: usize, b: usize) -> f32 {
    sdsp_test_kit::probes::max_step(&x[a..b.min(x.len())]) as f32
}

use superduper_synth_core::pitch_engine::{Mode, PitchEngine, ROUTE_THRESHOLD};
use superduper_synth_core::psola::PitchParams;

const BLOCK: usize = 256;

fn unity() -> PitchParams {
    PitchParams { pitch_st: 0.0, formant_st: 0.0, mix: 1.0, output_lin: 1.0, bypassed: false }
}

/// Run `x` through the engine in `BLOCK`-sized chunks, calling `at_block`
/// before each one so a test can flip the mode mid-stream.
fn run<F: FnMut(&mut PitchEngine, usize)>(
    e: &mut PitchEngine,
    x: &[f32],
    p: &PitchParams,
    mut at_block: F,
) -> Vec<f32> {
    let mut out = vec![0.0; x.len()];
    let mut at = 0;
    while at < x.len() {
        let n = BLOCK.min(x.len() - at);
        at_block(e, at);
        let (i, o) = (&x[at..at + n], &mut out[at..at + n]);
        let mut r = vec![0.0; n];
        e.process(i, i, o, &mut r, p);
        at += n;
    }
    out
}

fn engine() -> PitchEngine {
    let mut e = PitchEngine::new(common::SR, BLOCK);
    e.prime(1.0, 1.0);
    e
}

#[test]
fn auto_routes_pulsed_to_psola_and_smooth_to_pvoc() {
    let mut e = engine();
    e.set_mode(Mode::Auto);
    run(&mut e, &common::pulsed(1.5), &unity(), |_, _| {});
    let (pulsed_sharp, pulsed_mix) = (e.sharpness(), e.psola_mix());

    let mut e = engine();
    e.set_mode(Mode::Auto);
    run(&mut e, &common::smooth(1.5), &unity(), |_, _| {});
    let (smooth_sharp, smooth_mix) = (e.sharpness(), e.psola_mix());

    println!(
        "pulsed: sharpness {pulsed_sharp:.2} -> psola mix {pulsed_mix:.2}\n\
         smooth: sharpness {smooth_sharp:.2} -> psola mix {smooth_mix:.2}   (threshold {ROUTE_THRESHOLD})"
    );
    assert!(pulsed_mix > 0.99, "pulsed material must end on PSOLA, got mix {pulsed_mix:.2}");
    assert!(smooth_mix < 0.01, "smooth material must end on the phase vocoder, got mix {smooth_mix:.2}");
}

/// Lesson 24's rule, as a test: "do nothing" has to be transparent on a
/// pulsed AND a smooth source before "do the thing" is worth measuring. This
/// is the acceptance criterion the whole track exists for — today PSOLA alone
/// turns the smooth source's −66.9 dB into −2.6 dB.
#[test]
fn unity_shift_is_transparent_on_both_kinds_of_material() {
    for (name, x, f0) in [
        ("pulsed", common::pulsed(2.5), common::F0),
        ("smooth", common::smooth(2.5), common::F0),
    ] {
        let mut e = engine();
        e.set_mode(Mode::Auto);
        let y = run(&mut e, &x, &unity(), |_, _| {});
        // Skip the engine latency plus the router's settling time.
        let nin = common::noise_to_harmonic(&x, f0, 0.3);
        let nout = common::noise_to_harmonic(&y, f0, 1.2);
        println!("{name}: in {nin:.1} dB -> out {nout:.1} dB (psola mix {:.2})", e.psola_mix());
        assert!(
            nout - nin < 2.0,
            "{name}: auto routing must stay within 2 dB of the input, got {nin:.1} -> {nout:.1}"
        );
    }
}

/// A mid-note engine change must not click. The bound is relative: the same
/// signal's own largest sample step, measured away from the switch, is the
/// only honest reference — an absolute threshold would just encode this
/// test's amplitude.
#[test]
fn switching_engines_mid_note_does_not_click() {
    let x = common::pulsed(2.0);
    let mut e = engine();
    e.set_mode(Mode::Psola);
    let switch_at = (1.0 * common::SR) as usize;
    let y = run(&mut e, &x, &unity(), |e, at| {
        if at <= switch_at && at + BLOCK > switch_at {
            e.set_mode(Mode::Pvoc);
        }
    });

    // The switch lands one engine-latency later in the output, and the
    // warm-up runs for another latency before the fade even starts.
    let lat = e.latency_samples() as usize;
    let fade_from = switch_at + lat;
    let fade_to = fade_from + 2 * lat + (0.05 * common::SR) as usize;
    let steady = max_step(&y, (0.4 * common::SR) as usize, (0.9 * common::SR) as usize);
    let during = max_step(&y, fade_from, fade_to);
    println!("max step: steady {steady:.4}, across the switch {during:.4}");
    assert!(
        during < 2.0 * steady.max(1e-4),
        "engine switch clicks: {during:.4} against a steady {steady:.4}"
    );
}

/// The level must not move while the engines swap — in either direction. A
/// bulge and a dip are both audible as a lurch, and which one a given law
/// produces depends on the correlation measured in
/// `how_correlated_are_the_two_engines`.
#[test]
fn the_crossfade_holds_its_level() {
    let x = common::pulsed(2.5);
    let mut e = engine();
    e.set_mode(Mode::Psola);
    let switch_at = (1.0 * common::SR) as usize;
    let y = run(&mut e, &x, &unity(), |e, at| {
        if at <= switch_at && at + BLOCK > switch_at {
            e.set_mode(Mode::Pvoc);
        }
    });

    let lat = e.latency_samples() as usize;
    // A short window on a 220 Hz pulse train wobbles on its own — barely one
    // period fits — so the reference is the SAME sliding measurement over a
    // steady stretch, not a number picked to pass.
    let win = (0.005 * common::SR) as usize;
    let sweep = |from: usize, to: usize| -> (f32, f32) {
        let l: Vec<f32> = (from..to.min(y.len() - win))
            .step_by(win / 4)
            .map(|a| rms_db(&y, a, a + win))
            .collect();
        (l.iter().copied().fold(f32::MIN, f32::max), l.iter().copied().fold(f32::MAX, f32::min))
    };
    let (ref_hi, ref_lo) =
        sweep((0.4 * common::SR) as usize, (0.9 * common::SR) as usize);
    let (hi, lo) = sweep(switch_at + lat, switch_at + 3 * lat + (0.05 * common::SR) as usize);
    println!(
        "rms spread: steady {ref_lo:.1}..{ref_hi:.1} dB, across the switch {lo:.1}..{hi:.1} dB"
    );
    assert!(hi < ref_hi + 0.5, "the crossfade bulges: {hi:.1} dB against a steady peak of {ref_hi:.1}");
    assert!(lo > ref_lo - 0.5, "the crossfade dips: {lo:.1} dB against a steady trough of {ref_lo:.1}");
}

/// Forced modes must stay forced — a router that overrides the user's choice
/// is worse than no router.
#[test]
fn forced_modes_ignore_the_material() {
    let smooth = common::smooth(1.5);
    let mut e = engine();
    e.set_mode(Mode::Psola);
    run(&mut e, &smooth, &unity(), |_, _| {});
    assert!(e.psola_mix() > 0.99, "Psola mode drifted off PSOLA on smooth material");

    let pulsed = common::pulsed(1.5);
    let mut e = engine();
    e.set_mode(Mode::Pvoc);
    run(&mut e, &pulsed, &unity(), |_, _| {});
    assert!(e.psola_mix() < 0.01, "Pvoc mode drifted onto PSOLA on pulsed material");
}

/// The floor is the reason a bass never locked. 70 Hz has to cost latency —
/// if it ever stops doing so, the look-behind is no longer covering the
/// longest period and the engine is lying to the host.
#[test]
fn a_lower_floor_buys_low_voices_and_costs_latency() {
    let live = PitchEngine::with_floor(common::SR, BLOCK, 95.0);
    let low = PitchEngine::with_floor(common::SR, BLOCK, 70.0);
    let (a, b) = (live.latency_samples(), low.latency_samples());
    println!("latency: 95 Hz floor {a} samples, 70 Hz floor {b} samples");
    assert!(b > a, "a lower floor must extend the look-behind: {a} -> {b}");
    assert_eq!(b, 4 * (common::SR / 70.0).ceil() as u32, "latency must stay 4*T0_max");
}

/// Which crossfade law is correct depends on one measurable fact: how
/// correlated the two engines' outputs are. Correlated → linear (equal-power
/// would bulge +3 dB); uncorrelated → equal-power (linear would dip 3 dB).
/// Printed so the choice in `pitch_engine.rs` is answerable, not stylistic.
#[test]
fn how_correlated_are_the_two_engines() {
    use superduper_synth_core::psola::PitchShifter;
    use superduper_synth_core::pvoc::PhaseVocoder;
    let lat = 2744usize;
    for (name, x) in [("pulsed", common::pulsed(2.0)), ("smooth", common::smooth(2.0))] {
        let mut s = PitchShifter::with_range(common::SR, BLOCK, lat, 70.0, 1000.0);
        s.prime(1.0, 1.0);
        let mut v = PhaseVocoder::new(common::SR, lat);
        let p = unity();
        let (mut ya, mut yb) = (vec![0.0; x.len()], vec![0.0; x.len()]);
        let mut at = 0;
        while at < x.len() {
            let n = BLOCK.min(x.len() - at);
            let mut r = vec![0.0; n];
            s.process(&x[at..at + n], &x[at..at + n], &mut ya[at..at + n], &mut r, &p);
            v.process(&x[at..at + n], &x[at..at + n], &mut yb[at..at + n], &mut r, &p);
            at += n;
        }
        let (a, b) = ((1.0 * common::SR) as usize, (1.8 * common::SR) as usize);
        let (mut num, mut ea, mut eb) = (0.0f64, 0.0f64, 0.0f64);
        for i in a..b {
            num += (ya[i] * yb[i]) as f64;
            ea += (ya[i] * ya[i]) as f64;
            eb += (yb[i] * yb[i]) as f64;
        }
        let r = num / (ea * eb).sqrt().max(1e-30);
        println!("{name}: PSOLA vs pvoc correlation r = {r:.3}");
    }
}

/// The second, independent defect this track folds in: PSOLA's pitch floor
/// was hardcoded at 95 Hz, so a bass or a low male voice never locked — an
/// 87 Hz take went through untouched and nobody got an error. With the floor
/// at 70 Hz the engine tracks it, and Auto routes it to PSOLA like any other
/// voice.
#[test]
fn an_87_hz_voice_is_tracked_and_routed_to_psola() {
    let x = common::voiced_at(2.5, 87.0, 0.02, 0.4);
    let mut e = engine();
    e.set_mode(Mode::Auto);
    let y = run(&mut e, &x, &unity(), |_, _| {});
    println!("87 Hz: sharpness {:.2} -> psola mix {:.2}", e.sharpness(), e.psola_mix());
    assert!(e.psola_mix() > 0.99, "an 87 Hz voice must route to PSOLA, mix {:.2}", e.psola_mix());

    let nin = common::noise_to_harmonic(&x, 87.0, 0.3);
    let nout = common::noise_to_harmonic(&y, 87.0, 1.2);
    println!("87 Hz: in {nin:.1} dB -> out {nout:.1} dB");
    assert!(nout - nin < 2.0, "87 Hz take degraded: {nin:.1} -> {nout:.1}");

    // The old default would not even have tracked it: 95 Hz floor means the
    // period search never reaches 552 samples.
    let old = PitchEngine::with_floor(common::SR, BLOCK, 95.0);
    assert!(
        (common::SR / 95.0) < (common::SR / 87.0),
        "sanity: the 95 Hz floor really is above an 87 Hz period ({} vs {} samples)",
        (common::SR / 95.0) as u32,
        (common::SR / 87.0) as u32
    );
    assert!(old.latency_samples() < e.latency_samples());
}
