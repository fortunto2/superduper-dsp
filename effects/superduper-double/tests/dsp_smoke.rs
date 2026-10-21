//! Known-answer tests for the doubler engine — no CLAP, no host.
//!
//! Run: `cargo test --release -p superduper-double --test dsp_smoke -- --nocapture`

use sdsp_test_kit::signals;
use superduper_synth_core::doubler::{DoubleMode, Doubler, DoublerParams, DELAY_FLOOR};

const SR: f32 = 48_000.0;
const BLOCK: usize = 512;

fn sine(hz: f32, secs: f32, amp: f32) -> Vec<f32> {
    (0..(secs * SR) as usize)
        .map(|i| (core::f32::consts::TAU * hz * i as f32 / SR).sin() * amp)
        .collect()
}

/// Render through a fresh engine. Returns (L, R, per-block voice-0 readouts).
fn run(l: &[f32], r: &[f32], p: &DoublerParams) -> (Vec<f32>, Vec<f32>, Vec<Option<(f32, f32)>>) {
    let mut d = Doubler::new(SR, BLOCK);
    d.prime(p);
    let mut ol = vec![0.0; l.len()];
    let mut or = vec![0.0; l.len()];
    let mut reads = Vec::new();
    let mut pos = 0;
    while pos < l.len() {
        let e = (pos + BLOCK).min(l.len());
        d.process(&l[pos..e], &r[pos..e], &mut ol[pos..e], &mut or[pos..e], p);
        reads.push(d.voice_readout(0));
        pos = e;
    }
    (ol, or, reads)
}

/// One double, alone on the left channel (dry off, hard left).
fn solo_voice(p: DoublerParams) -> DoublerParams {
    DoublerParams { voices: 1, dry: 0.0, spread: 0.0, level: 1.0, ..p }
}

/// Mean frequency over `x` from interpolated positive-going zero crossings.
fn zc_freq(x: &[f32]) -> f32 {
    let mut first = None;
    let mut last = 0.0f32;
    let mut count = 0;
    for i in 1..x.len() {
        if x[i - 1] < 0.0 && x[i] >= 0.0 {
            let t = (i - 1) as f32 + x[i - 1] / (x[i - 1] - x[i]);
            if first.is_none() {
                first = Some(t);
            } else {
                count += 1;
            }
            last = t;
        }
    }
    let f = first.expect("no crossings");
    count as f32 * SR / (last - f)
}

fn cents(f: f32, reference: f32) -> f32 {
    1200.0 * (f / reference).log2()
}

fn corr(a: &[f32], b: &[f32]) -> f32 {
    let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
    for (&x, &y) in a.iter().zip(b) {
        ab += (x * y) as f64;
        aa += (x * x) as f64;
        bb += (y * y) as f64;
    }
    (ab / (aa.sqrt() * bb.sqrt()).max(1e-12)) as f32
}

#[test]
fn mix_zero_and_voices_zero_are_the_delayed_input_bit_for_bit() {
    let l = signals::voiced(1.5);
    let mut r: Vec<f32> = l.iter().map(|v| -0.7 * v).collect();
    r[1000] = -0.0;
    let lat = Doubler::new(SR, BLOCK).latency_samples() as usize;
    for p in [
        DoublerParams { mix: 0.0, ..Default::default() },
        DoublerParams { voices: 0, ..Default::default() },
        DoublerParams { mix: 0.0, mode: DoubleMode::Tuned, ..Default::default() },
    ] {
        let (ol, or, _) = run(&l, &r, &p);
        for i in 0..l.len() {
            let (el, er) = if i < lat { (0.0, 0.0) } else { (l[i - lat], r[i - lat]) };
            assert_eq!(ol[i].to_bits(), el.to_bits(), "L differs at {i} for {p:?}");
            assert_eq!(or[i].to_bits(), er.to_bits(), "R differs at {i} for {p:?}");
        }
    }
    println!("latency = {lat} samples ({:.1} ms)", lat as f32 * 1000.0 / SR);
}

#[test]
fn natural_double_wanders_inside_pitch_var() {
    let var = 10.0;
    let x = sine(220.0, 8.0, 0.5);
    let p = solo_voice(DoublerParams { pitch_var_cents: var, ..Default::default() });
    let (out, _, _) = run(&x, &x, &p);
    let win = 4096;
    let mut readings = Vec::new();
    let mut i = (0.5 * SR) as usize;
    while i + win < out.len() {
        readings.push(cents(zc_freq(&out[i..i + win]), 220.0));
        i += win;
    }
    let lo = readings.iter().cloned().fold(f32::INFINITY, f32::min);
    let hi = readings.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    println!("pitch offsets over {} windows: {lo:+.2} .. {hi:+.2} ct (var ±{var})", readings.len());
    assert!(lo >= -(var + 1.5) && hi <= var + 1.5, "out of ±{var} ct: {lo}..{hi}");
    assert!(hi - lo > 3.0, "pitch should wander, not sit still: {lo}..{hi}");
}

/// Lag of `out` behind `inp` around sample `at`, searched over `lo..hi`.
fn lag_at(inp: &[f32], out: &[f32], at: usize, lo: usize, hi: usize) -> usize {
    let w = 2048;
    let seg = &out[at..at + w];
    (lo..hi)
        .max_by(|&a, &b| {
            let ca = corr(seg, &inp[at - a..at - a + w]);
            let cb = corr(seg, &inp[at - b..at - b + w]);
            ca.partial_cmp(&cb).unwrap()
        })
        .unwrap()
}

#[test]
fn natural_double_timing_moves_inside_the_timing_range() {
    let timing = 40.0;
    // Noise, so the cross-correlation has exactly one answer: on a held note
    // every lag one period apart matches too. With nothing to track, the grain
    // grid runs at the tracker's 150 Hz default, so a grain's copy offset is
    // the delay to within half of that period.
    let x = sdsp_test_kit::probes::noise((8.0 * SR) as usize, 0.5);
    let half_t0_ms = 0.5 * 1000.0 / 150.0;
    let p = solo_voice(DoublerParams {
        timing_ms: timing,
        pitch_var_cents: 0.0,
        ..Default::default()
    });
    let (out, _, reads) = run(&x, &x, &p);
    // 1. What the engine says it is doing.
    let ms: Vec<f32> = reads.iter().skip(20).map(|r| r.expect("voice on").0).collect();
    let lo = ms.iter().cloned().fold(f32::INFINITY, f32::min);
    let hi = ms.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    println!("readout delay {lo:.1} .. {hi:.1} ms (range {:.1}..{timing})", DELAY_FLOOR * timing);
    assert!(lo >= DELAY_FLOOR * timing - 0.5 && hi <= timing + 0.5, "{lo}..{hi}");
    assert!(hi - lo > 8.0, "timing must drift, not sit: {lo}..{hi}");
    // 2. What the audio says: cross-correlate the double against the input.
    let lat = Doubler::new(SR, BLOCK).latency_samples() as usize;
    let t0_max = (SR / 70.0).ceil() as usize;
    let (mut llo, mut lhi) = (f32::INFINITY, f32::NEG_INFINITY);
    let mut at = SR as usize;
    while at + 4096 < out.len() {
        let l = lag_at(&x, &out, at, lat - 400, lat + (0.06 * SR) as usize);
        let measured = (l as f32 - lat as f32) * 1000.0 / SR;
        // The grain that lands at `at` was cut when the input was t0_max past it.
        let said = reads[(at - t0_max) / BLOCK].unwrap().0;
        println!("  t={:.2}s measured {measured:5.1} ms, engine said {said:5.1} ms", at as f32 / SR);
        assert!((measured - said).abs() <= half_t0_ms + 1.0, "audio lag {measured} vs engine {said}");
        llo = llo.min(measured);
        lhi = lhi.max(measured);
        at += (0.25 * SR) as usize;
    }
    println!("measured lag {llo:.1} .. {lhi:.1} ms");
    let slack = half_t0_ms + 1.0;
    assert!(llo >= DELAY_FLOOR * timing - slack && lhi <= timing + slack, "{llo}..{lhi}");
    assert!(lhi - llo > 6.0, "measured lag barely moves: {llo}..{lhi}");
}

/// Power-weighted centroid of `x`'s spectrum inside `lo..hi` Hz.
fn band_centroid(x: &[f32], lo: f32, hi: f32) -> f32 {
    use realfft::RealFftPlanner;
    let n = x.len();
    let mut buf: Vec<f32> = x
        .iter()
        .enumerate()
        .map(|(i, v)| v * (0.5 - 0.5 * (core::f32::consts::TAU * i as f32 / n as f32).cos()))
        .collect();
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
    let mut spec = fft.make_output_vec();
    fft.process(&mut buf, &mut spec).unwrap();
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for (k, c) in spec.iter().enumerate() {
        let f = k as f32 * SR / n as f32;
        if f >= lo && f <= hi {
            let pw = c.norm_sqr() as f64;
            num += f as f64 * pw;
            den += pw;
        }
    }
    (num / den.max(1e-30)) as f32
}

const FORMANT_BANDS: [(f32, f32); 4] = [(400.0, 1000.0), (850.0, 1400.0), (2000.0, 3000.0), (200.0, 4000.0)];

fn formant_drift(formant_st: f32) -> f32 {
    let x = signals::voiced(3.0);
    let p = solo_voice(DoublerParams { pitch_var_cents: 10.0, formant_st, ..Default::default() });
    let (out, _, _) = run(&x, &x, &p);
    let a = (1.0 * SR) as usize;
    let len = 1 << 16;
    let mut worst = 0.0f32;
    for (lo, hi) in FORMANT_BANDS {
        let ci = band_centroid(&x[a..a + len], lo, hi);
        let co = band_centroid(&out[a..a + len], lo, hi);
        let e = (co / ci - 1.0).abs();
        println!("  formant {formant_st:+}: {lo:.0}-{hi:.0} Hz centroid in {ci:.0} out {co:.0} ({:+.1} %)", (co / ci - 1.0) * 100.0);
        worst = worst.max(e);
    }
    worst
}

#[test]
fn natural_double_keeps_the_formants() {
    let kept = formant_drift(0.0);
    assert!(kept < 0.04, "formant envelope moved {:.1} %", kept * 100.0);
    // The measurement has teeth: asking for a +3 st formant moves it.
    let moved = formant_drift(3.0);
    assert!(moved > 0.06, "a +3 st formant shift should register: {:.1} %", moved * 100.0);
}

#[test]
fn tuned_double_snaps_a_sharp_a3_to_220() {
    let f_in = 220.0 * 2f32.powf(23.0 / 1200.0);
    let x = sine(f_in, 3.0, 0.5);
    let p = solo_voice(DoublerParams {
        mode: DoubleMode::Tuned,
        retune_ms: 20.0,
        pitch_var_cents: 25.0, // must be ignored in Tuned
        ..Default::default()
    });
    let (out, _, _) = run(&x, &x, &p);
    for k in 0..6 {
        let a = (1.0 * SR) as usize + k * 8192;
        let c = cents(zc_freq(&out[a..a + 8192]), 220.0);
        println!("tuned double at {:.2} s: {c:+.2} ct from 220 Hz (input +23)", a as f32 / SR);
        assert!(c.abs() < 3.0, "tuned double off by {c} ct");
    }
}

#[test]
fn tuned_lead_snaps_the_lead_and_the_doubles_stay_natural() {
    let f_in = 220.0 * 2f32.powf(23.0 / 1200.0);
    let x = sine(f_in, 3.0, 0.5);
    let p = DoublerParams { mode: DoubleMode::TunedLead, voices: 0, retune_ms: 20.0, ..Default::default() };
    let (out, _, _) = run(&x, &x, &p);
    let a = (1.5 * SR) as usize;
    let c = cents(zc_freq(&out[a..a + 16384]), 220.0);
    println!("tuned lead: {c:+.2} ct from 220 Hz (input +23)");
    assert!(c.abs() < 3.0, "tuned lead off by {c} ct");
    // A natural double next to it still sits near the INPUT pitch (± var).
    let p = solo_voice(DoublerParams { mode: DoubleMode::TunedLead, pitch_var_cents: 5.0, ..Default::default() });
    let (out, _, _) = run(&x, &x, &p);
    let c = cents(zc_freq(&out[a..a + 16384]), f_in);
    assert!(c.abs() < 6.5, "double in Tuned Lead should stay natural: {c} ct from the input");
}

#[test]
fn voices_and_seeds_are_decorrelated() {
    let x = signals::voiced(4.0);
    let a = SR as usize;
    // Two voices of one seed, hard L and hard R.
    let p = DoublerParams { voices: 2, dry: 0.0, spread: 1.0, ..Default::default() };
    let (l, r, _) = run(&x, &x, &p);
    let c2 = corr(&l[a..], &r[a..]);
    // The same single voice under two seeds.
    let (s1, _, _) = run(&x, &x, &solo_voice(DoublerParams { seed: 1, ..Default::default() }));
    let (s2, _, _) = run(&x, &x, &solo_voice(DoublerParams { seed: 2, ..Default::default() }));
    let cs = corr(&s1[a..], &s2[a..]);
    println!("corr voice1/voice2 = {c2:.3}, seed1/seed2 = {cs:.3}");
    assert!(c2 < 0.9 && cs < 0.9, "{c2} {cs}");
    // And a seed is reproducible.
    let (s1b, _, _) = run(&x, &x, &solo_voice(DoublerParams { seed: 1, ..Default::default() }));
    assert_eq!(s1, s1b);
}

#[test]
fn silence_stays_silent_and_everything_is_finite() {
    let x = vec![0.0f32; (2.0 * SR) as usize];
    for mode in [DoubleMode::Natural, DoubleMode::Tuned, DoubleMode::TunedLead] {
        let p = DoublerParams { mode, voices: 4, ..Default::default() };
        let (l, r, _) = run(&x, &x, &p);
        assert!(l.iter().chain(&r).all(|v| *v == 0.0), "{mode:?} made sound from silence");
    }
    let n = sdsp_test_kit::probes::noise((2.0 * SR) as usize, 0.9);
    let (l, r, _) = run(&n, &n, &DoublerParams { voices: 4, formant_st: 4.0, ..Default::default() });
    assert!(l.iter().chain(&r).all(|v| v.is_finite() && v.abs() < 8.0));
}

