//! Restoration measured against known clean signals: the clean part is synthesised, so the
//! residual after processing is exactly what the processor failed to remove (or damaged).

use superduper_restore::{process_offline, Mode, Restorer, Settings};

const SR: f32 = 48_000.0;

/// xorshift32 → uniform in [-1, 1).
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

fn white(n: usize, amp: f32, seed: u32) -> Vec<f32> {
    let mut r = Rng(seed);
    (0..n).map(|_| r.next() * amp).collect()
}

fn tone(n: usize, f: f32, amp: f32) -> Vec<f32> {
    (0..n).map(|i| amp * (std::f32::consts::TAU * f * i as f32 / SR).sin()).collect()
}

fn energy(x: &[f32]) -> f64 {
    x.iter().map(|&v| v as f64 * v as f64).sum()
}

fn db(x: f64) -> f64 {
    10.0 * x.max(1e-30).log10()
}

/// SNR of `y` against the known clean `s` over `range` (residual = y − s).
fn snr_db(s: &[f32], y: &[f32], range: std::ops::Range<usize>) -> f64 {
    let sig = energy(&s[range.clone()]);
    let res: f64 = range.map(|i| (y[i] as f64 - s[i] as f64).powi(2)).sum();
    db(sig / res)
}

/// Speech-like program: 1 kHz + 2.5 kHz partials in 0.5 s bursts with 0.5 s gaps, so the
/// adaptive floor sees noise in the gaps the way it does between words.
fn bursts(n: usize) -> Vec<f32> {
    let a = tone(n, 1000.0, 0.2);
    let b = tone(n, 2500.0, 0.1);
    let half = SR as usize / 2;
    (0..n).map(|i| if (i / half) % 2 == 1 { a[i] + b[i] } else { 0.0 }).collect()
}

fn run(settings: Settings, x: &[f32], profile: Option<(usize, usize)>) -> Vec<f32> {
    let mut ch = vec![x.to_vec()];
    process_offline(&mut ch, SR, settings, profile);
    ch.pop().unwrap()
}

fn spectral(reduction_db: f32) -> Settings {
    Settings { mode: Mode::Spectral, reduction_db, ..Settings::default() }
}

/// Measured 2026-10-26 (release): SNR 11.8 → 25.6 dB, +13.8 dB. The floor below sits a few dB
/// under the measurement, so a regression fails.
#[test]
fn spectral_adaptive_raises_snr_on_bursts_in_white_noise() {
    let n = SR as usize * 8;
    let s = bursts(n);
    let noise = white(n, 0.05, 7);
    let x: Vec<f32> = s.iter().zip(&noise).map(|(a, b)| a + b).collect();
    let y = run(spectral(20.0), &x, None);
    // Skip the first 3 s: the minimum-statistics window needs ~1.5 s to find the floor.
    let r = SR as usize * 3..n;
    let (before, after) = (snr_db(&s, &x, r.clone()), snr_db(&s, &y, r));
    eprintln!("adaptive: SNR {before:.1} → {after:.1} dB (+{:.1})", after - before);
    assert!(after - before > 10.0, "adaptive gating gained only {:.1} dB", after - before);
}

/// A stationary tone is exactly what minimum statistics mistakes for noise; a profile learned
/// from a noise-only lead-in keeps it. This is the reason the learned mode exists. Measured
/// 2026-10-26: adaptive keeps −19.6 dB of the tone, learned 0.0 dB with +17.6 dB SNR.
#[test]
fn spectral_learned_profile_keeps_a_held_tone_and_still_cleans() {
    let lead = SR as usize;
    let n = SR as usize * 6;
    let held: Vec<f32> = tone(n, 1000.0, 0.2).iter().enumerate().map(|(i, &v)| if i < lead { 0.0 } else { v }).collect();
    let noise = white(n, 0.05, 11);
    let x: Vec<f32> = held.iter().zip(&noise).map(|(a, b)| a + b).collect();
    let r = SR as usize * 3..n;

    let adaptive = run(spectral(20.0), &x, None);
    let learned = run(spectral(20.0), &x, Some((0, lead)));
    let tone_kept = |y: &[f32]| db(energy(&y[r.clone()]) / energy(&held[r.clone()]));
    let (a, l) = (tone_kept(&adaptive), tone_kept(&learned));
    let gain = snr_db(&held, &learned, r.clone()) - snr_db(&held, &x, r.clone());
    eprintln!("held tone: adaptive keeps {a:.1} dB, learned keeps {l:.1} dB; learned SNR +{gain:.1} dB");
    assert!(a < -6.0, "adaptive was expected to eat a held tone, kept {a:.1} dB");
    assert!(l > -1.5, "learned profile should keep the held tone, kept {l:.1} dB");
    assert!(gain > 12.0, "learned profile gained only {gain:.1} dB SNR");
}

#[test]
fn silence_in_is_silence_out_in_every_mode() {
    for mode in Mode::ALL {
        let y = run(Settings { mode, ..Settings::default() }, &vec![0.0; SR as usize * 2], None);
        assert!(y.iter().all(|&v| v == 0.0), "{mode:?}: silence came out non-zero");
    }
}

/// Neural is not built yet: it must be an exact delayed identity, never a broken effect.
#[test]
fn neural_mode_passes_audio_untouched_with_the_common_latency() {
    let x = white(20_000, 0.5, 3);
    let mut r = Restorer::new(SR, 1);
    r.set(Settings { mode: Mode::Neural, ..Settings::default() });
    let lat = r.latency();
    let mut y = x.clone();
    r.process(&mut [&mut y]);
    for i in lat..x.len() {
        assert_eq!(y[i], x[i - lat], "sample {i}");
    }
    assert!(!Mode::Neural.is_implemented());
}

#[test]
fn every_mode_reports_the_same_latency_and_offline_is_aligned() {
    let x = bursts(SR as usize * 2);
    // Zero reduction is identity for Spectral: offline output must line up sample for sample.
    let y = run(spectral(0.0), &x, None);
    let err = x.iter().zip(&y).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(err < 1e-4, "offline output misaligned or altered, max err {err}");
    let neural = run(Settings { mode: Mode::Neural, ..Settings::default() }, &x, None);
    assert_eq!(neural, x, "offline Neural must be identity");
}

/// Syllable-like dry bursts (150 ms every 500 ms) through an exponential diffuse tail (RT60
/// 0.8 s). Between syllables what remains is reverb; Dereverb should cut it and leave the
/// syllables near their level. A *sustained* stationary sound is cut too (the model predicts
/// its own past as reverb, measured −6 dB on a 0.5 s noise burst at 100 %), which is why this
/// is a voice mode and the test uses voice-shaped timing.
#[test]
fn dereverb_cuts_the_tail_between_syllables() {
    let n = SR as usize * 6;
    let period = SR as usize / 2;
    let syl = 3 * SR as usize / 20;
    let rt60 = 0.8f32;
    let mut rng = Rng(5);
    let ir: Vec<f32> = (0..SR as usize)
        .map(|i| if i == 0 { 1.0 } else { rng.next() * 10f32.powf(-3.0 * (i as f32 / SR) / rt60) * 0.05 })
        .collect();
    let dry: Vec<f32> = white(n, 0.3, 9).iter().enumerate().map(|(i, &v)| if i % period < syl { v } else { 0.0 }).collect();
    let mut wet = vec![0.0f32; n];
    for (i, &d) in dry.iter().enumerate().filter(|(_, d)| **d != 0.0) {
        for (j, &h) in ir.iter().enumerate().take(n - i) {
            wet[i + j] += d * h;
        }
    }
    let y = run(Settings { mode: Mode::Dereverb, amount_pct: 100.0, rt60_s: rt60, ..Settings::default() }, &wet, None);
    // Syllables 4.. (after 2 s of warm-up): tail = 50..300 ms after each one ends.
    let (mut tail_in, mut tail_out, mut body_in, mut body_out) = (0.0, 0.0, 0.0, 0.0);
    for k in 4..n / period {
        let s0 = k * period;
        let tail = s0 + syl + SR as usize / 20..s0 + syl + 3 * SR as usize / 10;
        tail_in += energy(&wet[tail.clone()]);
        tail_out += energy(&y[tail]);
        body_in += energy(&wet[s0..s0 + syl]);
        body_out += energy(&y[s0..s0 + syl]);
    }
    let (tail_cut, body_change) = (db(tail_out / tail_in), db(body_out / body_in));
    eprintln!("dereverb: tail {tail_cut:.1} dB, syllables {body_change:.1} dB");
    assert!(tail_cut < -6.0, "tail only cut {tail_cut:.1} dB");
    assert!(body_change > -3.0, "syllables lost {body_change:.1} dB");
}
