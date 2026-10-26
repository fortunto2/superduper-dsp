//! Neural mode (RNNoise via nnnoiseless). Only built with `--features neural`.
#![cfg(feature = "neural")]

mod common;
use common::*;
use superduper_restore::{process_offline, Mode, Restorer, Settings};

fn neural(mix_pct: f32) -> Settings {
    Settings { mode: Mode::Neural, mix_pct, ..Settings::default() }
}

fn offline(x: &[f32], s: Settings, sr: f32) -> Vec<f32> {
    let mut ch = vec![x.to_vec()];
    process_offline(&mut ch, sr, s, None);
    ch.pop().unwrap()
}

/// Lag (in samples, `y` relative to `x`) of the cross-correlation peak within ±`max`.
fn best_lag(x: &[f32], y: &[f32], max: i64) -> i64 {
    let n = x.len() as i64;
    let (mut best, mut at) = (f64::MIN, 0);
    for lag in -max..=max {
        let c: f64 = (max..n - max).map(|i| x[i as usize] as f64 * y[(i + lag) as usize] as f64).sum();
        if c > best {
            best = c;
            at = lag;
        }
    }
    at
}

/// Measured 2026-10-26 (release): SNR 9.6 → 18.5 dB (+8.9), noise between syllables −18.8 dB.
#[test]
fn neural_raises_snr_on_a_voiced_talker_in_noise() {
    let n = SR as usize * 6;
    let s = voiced(n);
    let mut rng = Rng(17);
    let x: Vec<f32> = s.iter().map(|v| v + rng.next() * 0.03).collect();
    let y = offline(&x, neural(100.0), SR);
    let r = SR as usize..n;
    let snr = |z: &[f32]| {
        let res: f64 = r.clone().map(|i| (z[i] as f64 - s[i] as f64).powi(2)).sum();
        db(energy(&s[r.clone()]) / res)
    };
    let (before, after) = (snr(&x), snr(&y));
    // Noise alone, measured in the gaps between syllables (where s ≈ 0).
    let gaps: Vec<usize> = r.clone().filter(|&i| s[i].abs() < 1e-6).collect();
    let gap_cut = db(gaps.iter().map(|&i| (y[i] as f64).powi(2)).sum::<f64>() / gaps.iter().map(|&i| (x[i] as f64).powi(2)).sum::<f64>());
    eprintln!("neural: SNR {before:.1} → {after:.1} dB (+{:.1}), noise in gaps {gap_cut:.1} dB", after - before);
    assert!(after - before > 6.0, "neural gained only {:.1} dB SNR", after - before);
    assert!(gap_cut < -14.0, "noise between syllables only cut {gap_cut:.1} dB");
}

#[test]
fn neural_offline_output_is_aligned_with_the_input() {
    let s = voiced(SR as usize * 3);
    let y = offline(&s, neural(100.0), SR);
    let lag = best_lag(&s, &y, 1200);
    assert!(lag.abs() <= 2, "neural offline output is {lag} samples off");
}

#[test]
fn neural_reports_the_common_latency() {
    let r = Restorer::new(SR, 2);
    assert!(r.neural_ready(), "48 kHz must run the network");
    assert_eq!(r.latency(), 2048, "Neural must not change the plugin's reported latency");
}

#[test]
fn neural_silence_in_is_silence_out() {
    let y = offline(&vec![0.0; SR as usize * 2], neural(100.0), SR);
    assert!(y.iter().all(|&v| v == 0.0));
}

/// RNNoise is trained at 48 kHz only; elsewhere the mode refuses (exact passthrough) and says so.
#[test]
fn neural_refuses_other_sample_rates_by_passing_through() {
    let sr = 44_100.0;
    let r = Restorer::new(sr, 1);
    assert!(!r.neural_ready());
    let mut rng = Rng(3);
    let x: Vec<f32> = (0..20_000).map(|_| rng.next() * 0.3).collect();
    assert_eq!(offline(&x, neural(100.0), sr), x);
}

/// Mix 0 % is the dry signal, aligned; a half mix lands between dry and full-wet.
#[test]
fn mix_blends_dry_and_processed() {
    let n = SR as usize * 3;
    let s = voiced(n);
    let mut rng = Rng(23);
    let x: Vec<f32> = s.iter().map(|v| v + rng.next() * 0.03).collect();
    assert_eq!(offline(&x, neural(0.0), SR), x, "mix 0 must be the dry signal");
    let full = offline(&x, neural(100.0), SR);
    let half = offline(&x, neural(50.0), SR);
    let err: f32 = (0..n).map(|i| (half[i] - 0.5 * (x[i] + full[i])).abs()).fold(0.0, f32::max);
    assert!(err < 1e-5, "half mix is not the average of dry and wet, max err {err}");
}

/// CPU: printed as a realtime factor (processing time / audio duration), stereo.
#[test]
fn neural_realtime_factor() {
    let n = SR as usize * 10;
    let s = voiced(n);
    let mut l = s.clone();
    let mut rr = s;
    let mut r = Restorer::new(SR, 2);
    r.set(neural(100.0));
    let t = std::time::Instant::now();
    for (a, b) in l.chunks_mut(512).zip(rr.chunks_mut(512)) {
        r.process(&mut [a, b]);
    }
    let rtf = t.elapsed().as_secs_f64() / 10.0;
    eprintln!("neural stereo realtime factor {rtf:.4} ({:.2} µs per 512-sample block)", rtf * 512.0 / SR as f64 * 1e6);
    assert!(rtf < 0.5, "neural too slow: rtf {rtf}");
}
