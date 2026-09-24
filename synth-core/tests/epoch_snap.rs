//! Does TD-PSOLA's per-grain epoch snap earn its place?
//!
//! `refine_epoch` moves each grain's READ point to the loudest sample within
//! ±T0/2 of the nominal mark, independently per grain. The write point is not
//! moved, so the read-to-write offset wanders from grain to grain, and on
//! material with no real glottal pulse the grains overlap-add at scrambled
//! phase. That is the defect `pitch_engine` was built to route around: a
//! −66.9 dB noise floor becoming −2.6 dB on a synth tone.
//!
//! A standalone model (`docs/plan/psola-engine-choice_20260827/evidence/`)
//! suggested simply dropping the snap recovers most of that at no cost
//! elsewhere. It could not test the one case where the snap plausibly earns
//! its keep: **moving pitch**. The engine drives grains from a smoothed
//! `cur_t0` out of YIN, so when the pitch slides the nominal mark drifts
//! against the real pulse, and snapping may be correcting that drift.
//!
//! So this runs the real engine, both ways, on stationary AND moving sources.
//!
//! ## The bar, fixed before the first run
//!
//! Dropping the snap is justified only if ALL of these hold. They are
//! constants below so the decision cannot be re-read off whatever the numbers
//! turn out to be.
//!
//! 1. On material the snap is FOR (pulsed, voiced, and both moving sources),
//!    unity-shift transparency must not get worse by more than
//!    [`TOLERANCE_DB`].
//! 2. On smooth material it must improve by at least [`REQUIRED_GAIN_DB`] —
//!    otherwise there is no reason to touch anything.
//! 3. Nothing may click: max sample step must not rise on any source, at any
//!    shift, by more than [`STEP_TOLERANCE`]×.
//!
//! Transparency is measured as [`unity_residual_db`] — how far the output sits
//! from a latency-aligned copy of the input. Unlike noise-to-harmonic it
//! assumes nothing about the signal, so it is the only figure comparable
//! across a held note and a glide.

use sdsp_test_kit::probes::max_step;
use sdsp_test_kit::signals::{
    breathy, glide, noise_to_harmonic, pulsed, render_blocks, smooth, take_from_env,
    unity_residual_db, vibrato, voiced, F0, SR,
};
use superduper_synth_core::pitch::{epoch_sharpness, EPOCH_SHARPNESS_THRESHOLD};
use superduper_synth_core::psola::{PitchParams, PitchShifter};

const BLOCK: usize = 256;
const FLOOR_HZ: f32 = 70.0;

/// How much worse the no-snap version may be on material with real epochs.
const TOLERANCE_DB: f32 = 1.0;
/// How much better it must be on smooth material to be worth the change.
const REQUIRED_GAIN_DB: f32 = 10.0;
/// How much the largest sample step may grow.
const STEP_TOLERANCE: f32 = 1.2;

fn render(x: &[f32], st: f32, snap: bool) -> Vec<f32> {
    let lat = PitchShifter::natural_latency_for(SR, FLOOR_HZ);
    let mut s = PitchShifter::with_range(SR, BLOCK, lat, FLOOR_HZ, 1000.0);
    s.set_epoch_snap(snap);
    s.prime(1.0, 1.0);
    let p =
        PitchParams { pitch_st: st, formant_st: 0.0, mix: 1.0, output_lin: 1.0, bypassed: false };
    let mut out = vec![0.0; x.len()];
    let mut r = vec![0.0; BLOCK];
    let mut at = 0;
    while at < x.len() {
        let n = BLOCK.min(x.len() - at);
        r.resize(n, 0.0);
        let (i, o) = (&x[at..at + n], &mut out[at..at + n]);
        s.process(i, i, o, &mut r, &p);
        at += n;
    }
    out
}

/// Same render, but the snap is steered per block by the descriptor already
/// shipped for engine routing — measured on the INPUT, which is what a caller
/// like `PitchEngine` has to hand anyway.
fn render_gated(x: &[f32], st: f32) -> Vec<f32> {
    let lat = PitchShifter::natural_latency_for(SR, FLOOR_HZ);
    let mut s = PitchShifter::with_range(SR, BLOCK, lat, FLOOR_HZ, 1000.0);
    s.prime(1.0, 1.0);
    let p =
        PitchParams { pitch_st: st, formant_st: 0.0, mix: 1.0, output_lin: 1.0, bypassed: false };
    let mut r = vec![0.0; BLOCK];
    render_blocks(x, BLOCK, |at, i, o| {
        let t0 = s.current_period().round() as usize;
        let need = 9 * t0;
        if at >= need {
            let sharp = epoch_sharpness(&x[at - need..at], t0);
            s.set_epoch_snap(sharp >= EPOCH_SHARPNESS_THRESHOLD);
        }
        s.process(i, i, o, &mut r[..i.len()], &p);
    })
}

fn latency() -> usize {
    PitchShifter::natural_latency_for(SR, FLOOR_HZ)
}

struct Source {
    name: &'static str,
    x: Vec<f32>,
    /// True for material that has real glottal epochs — the snap's home turf,
    /// where it must not lose.
    pulsed_like: bool,
}

fn sources() -> Vec<Source> {
    let mut v = vec![
        Source { name: "pulsed", x: pulsed(2.5), pulsed_like: true },
        Source { name: "voiced", x: voiced(2.5), pulsed_like: true },
        Source { name: "glide +2st", x: glide(2.5, 2.0), pulsed_like: true },
        Source { name: "vibrato 5Hz", x: vibrato(2.5, 5.0, 50.0), pulsed_like: true },
        Source { name: "breathy", x: breathy(2.5), pulsed_like: false },
        Source { name: "smooth", x: smooth(2.5), pulsed_like: false },
    ];
    if let Some(x) = take_from_env("SDSP_VOCAL_TAKE", 2.5) {
        v.push(Source { name: "real take", x, pulsed_like: true });
    }
    v
}

/// The whole experiment in one table.
#[test]
fn snap_versus_no_snap() {
    let lat = PitchShifter::natural_latency_for(SR, FLOOR_HZ);
    println!(
        "\n{:<12} {:>10} {:>10} {:>8}   {:>9} {:>9} {:>7}",
        "source", "snap dB", "none dB", "delta", "step snap", "step none", "ratio"
    );

    let mut verdict_ok = true;
    let mut smooth_gain = 0.0f32;
    for s in sources() {
        let (a, b) = (render(&s.x, 0.0, true), render(&s.x, 0.0, false));
        let (ra, la) = unity_residual_db(&s.x, &a, lat, 0.4);
        let (rb, lb) = unity_residual_db(&s.x, &b, lat, 0.4);
        let delta = rb - ra; // negative = no-snap is more transparent

        // Clicks are a shift-domain question, so check the downshift the snap
        // is supposed to protect.
        let (da, db_) = (render(&s.x, -12.0, true), render(&s.x, -12.0, false));
        let from = (0.6 * SR) as usize;
        let (sa, sb) =
            (max_step(&da[from..]) as f32, max_step(&db_[from..]) as f32);
        let ratio = sb / sa.max(1e-9);

        println!(
            "{:<12} {ra:>10.1} {rb:>10.1} {delta:>8.1}   {sa:>9.4} {sb:>9.4} {ratio:>7.2}   \
             lag {la}/{lb}",
            s.name
        );

        if s.pulsed_like && delta > TOLERANCE_DB {
            println!("   ^ FAILS bar 1: {} loses {delta:.1} dB without the snap", s.name);
            verdict_ok = false;
        }
        if ratio > STEP_TOLERANCE {
            println!("   ^ FAILS bar 3: {} steps {ratio:.2}x larger without the snap", s.name);
            verdict_ok = false;
        }
        if s.name == "smooth" {
            smooth_gain = -delta;
        }
    }

    println!(
        "\nsmooth gain without the snap: {smooth_gain:.1} dB (need {REQUIRED_GAIN_DB:.0})"
    );
    let worth_it = smooth_gain >= REQUIRED_GAIN_DB;
    println!(
        "VERDICT: {}",
        match (verdict_ok, worth_it) {
            (true, true) => "the snap can go — every bar met",
            (true, false) => "no reason to change: the snap costs nothing measurable here",
            (false, _) => "KEEP the snap — it pays for itself somewhere above",
        }
    );

    // This test records the experiment; it fails only if the engine stops
    // doing what the shipped configuration is documented to do.
    let (smooth_snap, _) =
        unity_residual_db(&smooth(2.5), &render(&smooth(2.5), 0.0, true), lat, 0.4);
    assert!(
        smooth_snap > -20.0,
        "the shipped snap is supposed to be BAD on smooth material ({smooth_snap:.1} dB); \
         if it is now good, lesson 24 and pitch_engine both need revisiting"
    );
}

/// The unity table above cannot settle this on its own, and it is worth being
/// explicit about why: at α = 1 the analysis and synthesis marks are spaced
/// identically, so WITHOUT the snap each grain is read from exactly where it
/// is written and the engine degenerates to a passthrough. A −34 dB residual
/// there says "did nothing", not "did it well".
///
/// So this asks the question in the domain that matters: at a real shift,
/// which version leaves less junk on the harmonic grid? Stationary sources
/// only, because the metric needs one f0 — the moving sources are covered by
/// the click and shift-accuracy checks instead.
#[test]
fn the_shift_domain_is_where_it_counts() {
    println!("\n{:<10} {:>7}   {:>18}   {:>18}", "source", "shift", "snap dB", "none dB");
    let mut regressions = Vec::new();
    for (name, x) in [
        ("pulsed", pulsed(2.5)),
        ("voiced", voiced(2.5)),
        ("breathy", breathy(2.5)),
        ("smooth", smooth(2.5)),
    ] {
        for st in [0.25f32, 12.0] {
            let grid = F0 * (st / 12.0).exp2();
            let a = noise_to_harmonic(&render(&x, st, true), grid, 1.0);
            let b = noise_to_harmonic(&render(&x, st, false), grid, 1.0);
            println!("{name:<10} {st:>7.2}   {a:>18.1}   {b:>18.1}");
            if b > a + TOLERANCE_DB {
                regressions.push(format!("{name} @ {st} st: {a:.1} -> {b:.1} dB"));
            }
        }
    }
    if regressions.is_empty() {
        println!("no source gets worse without the snap at a real shift");
    } else {
        println!("WITHOUT the snap these get worse: {}", regressions.join("; "));
    }
}

/// The third option, and the one the numbers point at: keep the snap where it
/// pays and drop it where it does not, decided by the descriptor that already
/// exists for choosing engines. If this matches `snap` on pulsed material and
/// `none` on smooth, then one adaptive engine does what two engines and a
/// crossfade are currently doing — for mono, at least.
#[test]
fn gating_the_snap_by_the_descriptor() {
    println!(
        "\n{:<10} {:>7}   {:>8} {:>8} {:>8}   {:>10}",
        "source", "shift", "snap", "none", "gated", "gated vs best"
    );
    let mut worst = f32::MIN;
    for (name, x) in [
        ("pulsed", pulsed(2.5)),
        ("voiced", voiced(2.5)),
        ("breathy", breathy(2.5)),
        ("smooth", smooth(2.5)),
    ] {
        for st in [0.25f32, 12.0] {
            let grid = F0 * (st / 12.0).exp2();
            let a = noise_to_harmonic(&render(&x, st, true), grid, 1.0);
            let b = noise_to_harmonic(&render(&x, st, false), grid, 1.0);
            let g = noise_to_harmonic(&render_gated(&x, st), grid, 1.0);
            let best = a.min(b);
            println!("{name:<10} {st:>7.2}   {a:>8.1} {b:>8.1} {g:>8.1}   {:>10.1}", g - best);
            worst = worst.max(g - best);
        }
    }
    println!("\nworst case: gating is {worst:.1} dB off the better of the two fixed settings");
    println!(
        "{}",
        if worst < 1.0 {
            "GATING WORKS: one adaptive engine matches the best fixed choice everywhere"
        } else {
            "gating does not reach the best fixed choice — the router earns its keep"
        }
    );
}

/// Whatever the snap does to transparency, the shift itself must still work.
/// A "fix" that cleans up the residual by quietly not shifting is the failure
/// mode that killed the three earlier attempts (one measured +12 st as −16).
#[test]
fn dropping_the_snap_still_shifts() {
    for snap in [true, false] {
        for (name, x) in [("voiced", voiced(2.5)), ("smooth", smooth(2.5))] {
            let y = render(&x, 12.0, snap);
            // Spectral peak, not YIN: PSOLA leaves the smooth tone noisy
            // enough that the tracker octave-errors, and that would be the
            // detector failing rather than the shift.
            let from = (1.0 * SR) as usize;
            let seg = &y[from..from + (1.0 * SR) as usize];
            let spec = superduper_synth_core::analysis::magnitude_spectrum_db(seg);
            let bin = SR / seg.len() as f32;
            let peak = spec
                .iter()
                .enumerate()
                .filter(|(i, _)| {
                    let hz = *i as f32 * bin;
                    (300.0..600.0).contains(&hz)
                })
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(i, _)| i as f32 * bin)
                .unwrap_or(0.0);
            let cents = 1200.0 * (peak / 440.0).log2();
            println!("snap {snap:<5} {name:<7} +12 st -> peak {peak:6.1} Hz ({cents:+.0} cents)");
            assert!(
                cents.abs() < 60.0,
                "snap {snap}: {name} at +12 st peaked at {peak:.1} Hz, not 440"
            );
        }
    }
}

/// The gate's real payoff is in the mode where the ROUTER is switched off.
/// A user who forces Voice because they want the independent formant axis,
/// and points it at a synth pad, used to get +2.7 dB of noise-to-harmonic at
/// +12 st. Gating the snap makes that −33.4 dB without changing what Voice
/// does to an actual voice.
#[test]
fn forced_voice_survives_the_wrong_material() {
    use superduper_synth_core::pitch_engine::{Mode, PitchEngine};
    let run = |x: &[f32], st: f32| {
        let mut e = PitchEngine::new(SR, BLOCK);
        e.prime(1.0, 1.0);
        e.set_mode(Mode::Psola);
        let p = PitchParams {
            pitch_st: st,
            formant_st: 0.0,
            mix: 1.0,
            output_lin: 1.0,
            bypassed: false,
        };
        let mut r = vec![0.0; BLOCK];
        render_blocks(&x, BLOCK, |_, i, o| e.process(i, i, o, &mut r[..i.len()], &p))
    };
    for (name, x, bound) in
        [("smooth", smooth(2.5), -25.0f32), ("voiced", voiced(2.5), -18.0f32)]
    {
        for st in [0.25f32, 12.0] {
            let grid = F0 * (st / 12.0).exp2();
            let v = noise_to_harmonic(&run(&x, st), grid, 1.0);
            println!("forced Voice, {name:<7} @ {st:>5.2} st -> {v:6.1} dB (bound {bound})");
            assert!(v < bound, "forced Voice on {name} at {st} st: {v:.1} dB, want under {bound}");
        }
    }
}

/// A limitation the gate created, recorded rather than papered over.
///
/// `epoch_sharpness` is shift-blind, so Auto picks the engine from the
/// material alone. Before the gate that was fine: PSOLA wrecked smooth
/// material at every shift. Gated, PSOLA holds about −34 dB across the whole
/// range while the vocoder degrades with the shift, and they cross between 4
/// and 7 semitones:
///
/// ```text
/// smooth tone   shift   gated PSOLA   pvoc
///                0.00        -36.7   -66.9
///                0.25        -35.9   -53.8
///                4.00        -35.3   -36.1
///                7.00        -33.6   -32.2   <- crossover
///               12.00        -33.4   -17.9
///               24.00        -28.2    -2.3
/// ```
///
/// **Update 2026-09-24: the crossover is gone.** Laroche-Dolson identity
/// phase locking in `pvoc` removed the with-shift degradation the table
/// records (12 st: −17.9 → −50.2, 24 st: −2.3 → −38.0 measured by this
/// test), so the vocoder now beats gated PSOLA on smooth mono at EVERY
/// shift and Auto's "smooth → pvoc" routing is right across the range.
/// The shift-aware routing rule discussed above is therefore not needed.
/// This test keeps the claim honest in the other direction: if a pvoc
/// change brings the crossover back, Auto is silently routing to the worse
/// engine again above it, and this fails.
#[test]
fn pvoc_beats_psola_on_smooth_mono_at_every_shift() {
    use superduper_synth_core::pitch_engine::{Mode, PitchEngine};
    let x = smooth(2.5);
    let render_mode = |st: f32, m: Mode| {
        let mut e = PitchEngine::new(SR, BLOCK);
        e.prime(1.0, 1.0);
        e.set_mode(m);
        let p = PitchParams {
            pitch_st: st,
            formant_st: 0.0,
            mix: 1.0,
            output_lin: 1.0,
            bypassed: false,
        };
        let mut out = vec![0.0; x.len()];
        let mut at = 0;
        while at + BLOCK <= x.len() {
            let mut r = vec![0.0; BLOCK];
            let (i, o) = (&x[at..at + BLOCK], &mut out[at..at + BLOCK]);
            e.process(i, i, o, &mut r, &p);
            at += BLOCK;
        }
        noise_to_harmonic(&out, F0 * (st / 12.0).exp2(), 1.0)
    };
    println!("\n{:>7} {:>12} {:>8}   {}", "shift", "gated PSOLA", "pvoc", "Auto picks");
    let mut crossover = None;
    for st in [0.25f32, 4.0, 7.0, 12.0, 24.0] {
        let (a, b) = (render_mode(st, Mode::Psola), render_mode(st, Mode::Pvoc));
        println!("{st:>7.2} {a:>12.1} {b:>8.1}   {}", if a < b { "the worse one" } else { "correctly" });
        if a < b && crossover.is_none() {
            crossover = Some(st);
        }
    }
    if let Some(st) = crossover {
        panic!(
            "PSOLA overtakes pvoc again at {st} st — phase locking regressed, and \
             Auto now routes smooth material to the worse engine above that shift"
        );
    }
    println!("no crossover: pvoc wins at every shift, Auto's smooth→pvoc routing is right everywhere");
}
