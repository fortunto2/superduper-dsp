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
    sdsp_test_kit::probes::rms_db(&x[a..b.min(x.len())]) as f32
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
    let mut r = vec![0.0; BLOCK];
    sdsp_test_kit::signals::render_blocks(x, BLOCK, |at, i, o| {
        at_block(e, at);
        e.process(i, i, o, &mut r[..i.len()], p);
    })
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

/// What the floor actually buys, measured on the tracker rather than on the
/// noise floor. The distinction matters: at the old 95 Hz default an 87 Hz
/// voice reads as **150 Hz** — the tracker's own default, i.e. it never locked
/// at all, 943 cents out — while noise-to-harmonic barely moves (−17.7 vs
/// −18.6 dB), because the grains still overlap-add coherently, just at the
/// wrong period. An autotune driven by that number corrects to a wrong note
/// and nothing sounds broken until you listen.
///
/// This is the evidence for paying 42 → 57 ms of latency, and it is why the
/// floor is not a taste setting.
#[test]
fn the_floor_is_the_difference_between_locking_and_not() {
    for (f0, floor, want_locked) in
        [(87.0f32, 95.0f32, false), (87.0, 70.0, true), (220.0, 95.0, true)]
    {
        let x = common::voiced_at(2.5, f0, 0.02, 0.4);
        let mut e = PitchEngine::with_floor(common::SR, BLOCK, floor);
        e.prime(1.0, 1.0);
        run(&mut e, &x, &unity(), |_, _| {});
        let hz = e.tracked_hz();
        let cents = 1200.0 * (hz / f0).log2();
        println!("f0 {f0:5.0} Hz at a {floor:.0} Hz floor -> tracked {hz:6.1} Hz ({cents:+.0} cents)");
        if want_locked {
            assert!(cents.abs() < 50.0, "should have locked: {hz:.1} Hz for a {f0:.0} Hz voice");
        } else {
            assert!(
                cents.abs() > 300.0,
                "the 95 Hz floor is supposed to MISS an 87 Hz voice; if it now locks \
                 ({hz:.1} Hz), the 70 Hz floor is buying nothing and its 15 ms should go back"
            );
        }
    }
}

/// 70 Hz has to cost latency — if it ever stops doing so, the look-behind is
/// no longer covering the longest period and the engine is lying to the host.
#[test]
fn a_lower_floor_costs_latency() {
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
///
/// This asserts nothing — it is the measurement the choice in
/// `pitch_engine.rs` cites (r = 0.13 pulsed, 0.60 smooth), kept runnable so
/// the claim can be re-checked rather than trusted. `#[ignore]`d so CI does
/// not pay for a test that can never fail:
/// `cargo test -p superduper-synth-core --test pitch_engine -- --ignored --nocapture`
#[test]
#[ignore = "measurement, not a check — run explicitly to re-derive the crossfade law"]
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
}

/// Two holes found by the cleanup review, each a test that must go red first.
///
/// A host handing over more frames than it declared indexed past the scratch
/// buffers and panicked — the only stage in the chain that did, where PSOLA
/// uses masked indexing and the vocoder is per-sample.
#[test]
fn an_oversized_block_degrades_instead_of_panicking() {
    let x = common::pulsed(1.0);
    let mut e = PitchEngine::new(common::SR, BLOCK);
    e.prime(1.0, 1.0);
    e.set_mode(Mode::Auto);
    let big = BLOCK * 5;
    let mut out = vec![0.0; big];
    let mut r = vec![0.0; big];
    e.process(&x[..big], &x[..big], &mut out, &mut r, &unity());
    assert!(out.iter().all(|v| v.is_finite()), "oversized block produced non-finite output");
}

/// `reset()` cleared the vocoder, the history and the votes but not PSOLA,
/// which had no `reset()` at all — so a reset engine still carried the
/// previous take's rings and tracked pitch.
#[test]
fn reset_actually_resets_both_engines() {
    let a = common::voiced(1.5);
    let b = common::pulsed(1.5);
    let p = unity();

    // Run something else through it, then reset, then run `b`.
    let mut used = engine();
    used.set_mode(Mode::Psola);
    run(&mut used, &a, &p, |_, _| {});
    used.reset();
    used.prime(1.0, 1.0);
    let after_reset = run(&mut used, &b, &p, |_, _| {});

    // A fresh engine given the same input must agree.
    let mut fresh = engine();
    fresh.set_mode(Mode::Psola);
    let virgin = run(&mut fresh, &b, &p, |_, _| {});

    let from = (0.8 * common::SR) as usize;
    let diff = after_reset[from..]
        .iter()
        .zip(&virgin[from..])
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    println!("max |reset - fresh| over the settled tail: {diff:.2e}");
    assert!(diff < 1e-6, "a reset engine still differs from a fresh one by {diff:.2e}");
}

/// Every reading this engine publishes comes from PSOLA's tracker, so it has
/// to advance even when PSOLA is not the engine rendering. In forced Pvoc the
/// `!both` fast path fed only the vocoder and the tracker froze at whatever it
/// last saw — `tracked_hz()` reported a held note forever, and `route()`
/// measured sharpness at a stale period. Tune reads `tracked_hz()`, so with
/// `Engine = Phase` its correction stuck on one note.
#[test]
fn tracked_hz_follows_the_input_in_every_mode() {
    for mode in [Mode::Auto, Mode::Psola, Mode::Pvoc] {
        let mut x = common::smooth_at(1.5, 220.0);
        x.extend(common::smooth_at(1.5, 330.0));
        let mut e = engine();
        e.set_mode(mode);
        run(&mut e, &x, &unity(), |_, _| {});
        let hz = e.tracked_hz();
        println!("{mode:?}: after stepping 220 -> 330 Hz, tracked {hz:.1} Hz");
        assert!(
            (hz - 330.0).abs() < 15.0,
            "{mode:?}: tracker stuck at {hz:.1} Hz after the input moved to 330"
        );
    }
}

/// The block-rate pitch update must agree with the per-sample one at every
/// buffer size a host might use.
///
/// `observe()` originally scaled the one-pole coefficient linearly by block
/// length, which passes 1.0 at 500 frames and 2.0 at 1000. At a 1024-frame
/// buffer — the size this rig mixes at — the recursion diverged to −1.4e10,
/// and at 2048 to NaN, after which every grain was poisoned and the plugin
/// rendered silence for the rest of the session. It only ran in a forced Pvoc
/// route, so Pitch's Track mode and Tune's Phase mode were the way in.
#[test]
fn the_block_rate_pitch_update_is_stable_at_every_buffer_size() {
    for block in [64usize, 256, 512, 1024, 2048, 4096] {
        let x = common::smooth_at(2.0, 220.0);
        let mut e = PitchEngine::new(common::SR, block);
        e.prime(1.0, 1.0);
        e.set_mode(Mode::Pvoc);
        let p = unity();
        let mut out = vec![0.0; x.len()];
        let mut at = 0;
        while at + block <= x.len() {
            let mut r = vec![0.0; block];
            let (i, o) = (&x[at..at + block], &mut out[at..at + block]);
            e.process(i, i, o, &mut r, &p);
            at += block;
        }
        // The divergence lives in the smoothed period, not the raw tracker,
        // and it only shows once PSOLA renders again — so switch back and
        // listen, which is how a user meets it.
        e.set_mode(Mode::Psola);
        let mut back = vec![0.0; x.len()];
        let mut at = 0;
        while at + block <= x.len() {
            let mut r = vec![0.0; block];
            let (i, o) = (&x[at..at + block], &mut back[at..at + block]);
            e.process(i, i, o, &mut r, &p);
            at += block;
        }
        let tail = &back[back.len() / 2..];
        let rms = (tail.iter().map(|v| v * v).sum::<f32>() / tail.len() as f32).sqrt();
        println!("block {block:>5}: period {:8.1}, RMS after returning to Voice {rms:.4}",
                 e.psola_period());
        assert!(tail.iter().all(|v| v.is_finite()), "block {block}: non-finite output");
        assert!(rms > 0.01, "block {block}: Voice mode renders silence (RMS {rms:.4})");
    }
}

/// A gap between phrases is not evidence about the material.
///
/// `epoch_sharpness` reports 0.0 for silence by design, and feeding that
/// straight into the vote made an ordinary breath argue for the phase
/// vocoder: a locked-on voice lost the route after ~53 ms of silence and
/// needed ~101 ms of singing to win it back. Every entry — the transient
/// where the epoch snap and the independent formant axis matter most — was
/// rendered by the wrong engine, and Formant audibly changed character at
/// each one.
#[test]
fn a_gap_between_phrases_does_not_change_the_route() {
    let phrase = common::voiced(1.5);
    let gap = vec![0.0f32; (0.4 * common::SR) as usize];
    let mut x = phrase.clone();
    x.extend_from_slice(&gap);
    x.extend_from_slice(&phrase);

    let mut e = engine();
    e.set_mode(Mode::Auto);
    let mut lowest_after_gap = 1.0f32;
    let gap_start = phrase.len();
    run(&mut e, &x, &unity(), |e, at| {
        if at >= gap_start {
            lowest_after_gap = lowest_after_gap.min(e.psola_mix());
        }
    });
    println!(
        "voice -> 400 ms of silence -> voice: psola mix dipped to {lowest_after_gap:.2}, ended {:.2}",
        e.psola_mix()
    );
    assert!(
        lowest_after_gap > 0.99,
        "the route left PSOLA during a silent gap (mix fell to {lowest_after_gap:.2})"
    );
}
