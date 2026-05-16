//! Automated DSP smoke test for SuperDuper Reverb (Dattorro plate).
//!
//! Drives `PlateState::process_sample` directly with a known stereo input
//! and validates:
//!   1. The output has a decaying tail after the input goes silent
//!      (reverb actually produces something).
//!   2. The output differs measurably from the input
//!      (processing happened — not silent passthrough).
//!   3. Long-decay runs stay bounded (feedback gain is sane).
//!   4. The two output channels differ (stereo crossfeed is wired).
//!
//! Run with: `cargo test -p superduper-reverb --test dsp_smoke -- --nocapture`

use superduper_reverb::{PlateParams, PlateState};

const SR: f32 = 48_000.0;

fn params() -> PlateParams {
    PlateParams {
        sr: SR,
        size: 1.0,
        decay: 0.7,
        damp: 0.3,
        bandwidth: 0.85,
        predelay_ms: 0.0,
        modulation: 0.5,
    }
}

fn run(state: &mut PlateState, input: &[(f32, f32)]) -> Vec<(f32, f32)> {
    let p = params();
    input
        .iter()
        .map(|&(l, r)| state.process_sample(l, r, p))
        .collect()
}

fn rms_l(samples: &[(f32, f32)]) -> f32 {
    let n = samples.len() as f32;
    let s: f32 = samples.iter().map(|(l, _)| l * l).sum();
    (s / n).sqrt()
}

fn rms_r(samples: &[(f32, f32)]) -> f32 {
    let n = samples.len() as f32;
    let s: f32 = samples.iter().map(|(_, r)| r * r).sum();
    (s / n).sqrt()
}

fn peak(samples: &[(f32, f32)]) -> f32 {
    samples
        .iter()
        .map(|(l, r)| l.abs().max(r.abs()))
        .fold(0.0_f32, f32::max)
}

#[test]
fn impulse_produces_decaying_tail() {
    let mut state = PlateState::default();

    let mut input = vec![(0.0_f32, 0.0_f32); (SR * 2.0) as usize];
    for s in input.iter_mut().take(8) {
        *s = (1.0, 1.0);
    }

    let output = run(&mut state, &input);

    let tail = &output[200..];
    let tail_peak = peak(tail);
    let tail_rms_l = rms_l(tail);
    let tail_rms_r = rms_r(tail);

    println!(
        "tail peak={:.6}  rms L={:.6} R={:.6}",
        tail_peak, tail_rms_l, tail_rms_r
    );
    println!("first 32 wet samples:");
    for (i, (l, r)) in output[..32].iter().enumerate() {
        println!("  [{i}] L={:+.5}  R={:+.5}", l, r);
    }

    assert!(
        tail_peak > 1e-4,
        "tail is effectively silent (peak={}); DSP isn't reverberating",
        tail_peak
    );
    assert!(
        tail_rms_l > 1e-5 && tail_rms_r > 1e-5,
        "tail RMS too low (L={}, R={})",
        tail_rms_l,
        tail_rms_r
    );
}

#[test]
fn sine_input_is_modified() {
    let mut state = PlateState::default();

    let n = SR as usize;
    let input: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let phase = i as f32 * 2.0 * core::f32::consts::PI * 440.0 / SR;
            let s = phase.sin() * 0.5;
            (s, s)
        })
        .collect();

    let output = run(&mut state, &input);

    let half = n / 2;
    let in_tail = &input[half..];
    let out_tail = &output[half..];

    let diff_sum_sq: f32 = in_tail
        .iter()
        .zip(out_tail.iter())
        .map(|(&(il, _), &(ol, _))| {
            let d = il - ol;
            d * d
        })
        .sum();
    let diff_rms = (diff_sum_sq / in_tail.len() as f32).sqrt();

    println!("diff_rms (L) = {:.6}", diff_rms);
    assert!(
        diff_rms > 0.01,
        "output too close to input (diff_rms={}), reverb isn't acting on signal",
        diff_rms
    );
}

#[test]
fn long_decay_stays_bounded() {
    let mut state = PlateState::default();

    let mut input = vec![(0.0_f32, 0.0_f32); (SR * 10.0) as usize];
    for s in input.iter_mut().take(2048) {
        *s = (1.0, 1.0);
    }
    let output = run(&mut state, &input);
    let pk = peak(&output);
    println!("long-decay peak: {:.4}", pk);
    assert!(pk < 5.0, "reverb is unstable (peak={})", pk);
}

#[test]
fn stereo_channels_differ() {
    let mut state = PlateState::default();
    let mut input = vec![(0.0_f32, 0.0_f32); (SR * 0.5) as usize];
    for s in input.iter_mut().take(8) {
        *s = (1.0, 1.0);
    }
    let output = run(&mut state, &input);

    let tail = &output[1000..];
    let diff_sum_sq: f32 = tail.iter().map(|(l, r)| (l - r) * (l - r)).sum();
    let diff_rms = (diff_sum_sq / tail.len() as f32).sqrt();
    println!("L-R diff rms = {:.6}", diff_rms);
    assert!(
        diff_rms > 1e-4,
        "L and R outputs are identical (diff_rms={}) — crossfeed not working",
        diff_rms
    );
}

// ===========================================================================
// Ducking — envelope follower attenuates wet path when key signal is hot.
// ===========================================================================

#[test]
fn ducker_attenuates_when_key_is_loud() {
    use superduper_reverb::plate::Ducker;

    let mut d = Ducker::default();
    // Hammer the envelope with a loud key for ~500 ms so attack stage stabilises.
    let mut gain_loud = 1.0;
    for _ in 0..(SR as usize / 2) {
        gain_loud = d.process(1.0, 1.0, SR, 12.0, 5.0, 100.0);
    }
    // Now go silent for 500 ms — gain should recover toward 1.0 (after release).
    let mut gain_silent = 1.0;
    for _ in 0..(SR as usize / 2) {
        gain_silent = d.process(0.0, 0.0, SR, 12.0, 5.0, 100.0);
    }
    println!("duck gain: loud={:.4}, silent={:.4}", gain_loud, gain_silent);

    // At 12 dB amount with envelope ~1.0, expect ~ -12 dB ≈ 0.25 gain.
    assert!(
        gain_loud < 0.35,
        "ducker didn't attenuate (gain={}) — expected ~0.25",
        gain_loud
    );
    assert!(
        gain_silent > 0.9,
        "ducker didn't recover (gain={}) — release stage broken",
        gain_silent
    );
}

#[test]
fn ducker_amount_zero_is_unity() {
    use superduper_reverb::plate::Ducker;

    let mut d = Ducker::default();
    for _ in 0..1000 {
        let g = d.process(1.0, 1.0, SR, 0.0, 5.0, 100.0);
        assert!((g - 1.0).abs() < 1e-6, "amount=0 should mean no ducking (got {g})");
    }
}
