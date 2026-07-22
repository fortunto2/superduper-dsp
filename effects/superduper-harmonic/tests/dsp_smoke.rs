//! Smoke tests — drive the `HarmonicCleaner` DSP block directly (no CLAP).
//! RMS / peak / stability / no-NaN across sane and extreme settings.
//!
//! Run: `cargo test -p superduper-harmonic --test dsp_smoke -- --nocapture`

use superduper_harmonic::dsp::{
    taps_from_bandwidth, HarmonicCleaner, HarmonicParams, K_MAX, K_MIN, MODE_MEAN, MODE_MEDIAN,
};

const SR: f32 = 48_000.0;

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|&v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}
fn peak(x: &[f32]) -> f32 {
    x.iter().map(|v| v.abs()).fold(0.0, f32::max)
}

/// Harmonic drone at `f0` + a bit of broadband noise.
fn drone(n: usize, f0: f32, noise_amp: f32) -> Vec<f32> {
    use std::f32::consts::TAU;
    let mut s: u32 = 0xABCD_1234;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let mut v = 0.0;
            for h in 1..=12 {
                v += (0.5 / h as f32) * (TAU * f0 * h as f32 * t).sin();
            }
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let noise = (s as f32 / u32::MAX as f32) * 2.0 - 1.0;
            v * 0.25 + noise * noise_amp
        })
        .collect()
}

fn run(cleaner: &mut HarmonicCleaner, x: &[f32], p: &HarmonicParams) -> Vec<f32> {
    let n = x.len();
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    cleaner.process_stereo(x, x, &mut out_l, &mut out_r, p);
    out_l
}

#[test]
fn bandwidth_maps_to_tap_count() {
    // Narrow (low) = aggressive = most taps; wide (high) = gentle = fewest.
    assert_eq!(taps_from_bandwidth(0.0), K_MAX);
    assert_eq!(taps_from_bandwidth(1.0), K_MIN);
    let mid = taps_from_bandwidth(0.5);
    assert!((K_MIN..=K_MAX).contains(&mid));
    // Monotone: less bandwidth → not fewer taps.
    assert!(taps_from_bandwidth(0.2) >= taps_from_bandwidth(0.8));
}

#[test]
fn steady_drone_is_stable_and_finite() {
    let n = SR as usize; // 1 s
    let x = drone(n, 90.0, 0.03);
    let mut c = HarmonicCleaner::new(SR);
    let p = HarmonicParams::default();
    let out = run(&mut c, &x, &p);
    let tail = &out[8192..];
    assert!(tail.iter().all(|v| v.is_finite()), "non-finite output");
    assert!(peak(tail) <= 4.0, "output blew up: peak={}", peak(tail));
    assert!(rms(tail) > 1e-4, "output too quiet: rms={}", rms(tail));
    // The cleaner should be reporting a plausible locked f0.
    let f0 = c.detected_f0();
    println!("steady drone: detected f0 = {f0:.1} Hz, reduction = {:.2}", c.reduction());
    assert!((70.0..=140.0).contains(&f0), "f0 lock off: {f0:.1} Hz");
}

#[test]
fn extreme_params_do_not_nan() {
    let n = 24_000usize;
    let x = drone(n, 110.0, 0.1);
    for &(amount, bw, transient, mix, out_db, range) in &[
        (1.0f32, 0.0f32, 0.0f32, 1.0f32, 24.0f32, 40.0f32),
        (1.0, 1.0, 1.0, 1.0, -24.0, 200.0),
        (0.0, 0.5, 0.5, 0.0, 0.0, 60.0),
        (0.5, 0.5, 0.5, 0.5, 6.0, 90.0),
    ] {
        for mode in [MODE_MEDIAN, MODE_MEAN] {
            let mut c = HarmonicCleaner::new(SR);
            let p = HarmonicParams {
                amount,
                bandwidth: bw,
                transient,
                mix,
                output_lin: 10f32.powf(out_db / 20.0),
                range_hz: range,
                mode,
                bypassed: false,
            };
            let out = run(&mut c, &x, &p);
            assert!(out.iter().all(|v| v.is_finite()), "NaN/Inf at {amount},{bw},{transient},mode={mode}");
            assert!(peak(&out) <= 20.0, "runaway at {amount},{bw},{transient},mode={mode}: {}", peak(&out));
        }
    }
}

#[test]
fn bypass_passes_through() {
    let n = 8_000usize;
    let x = drone(n, 90.0, 0.05);
    let mut c = HarmonicCleaner::new(SR);
    let mut p = HarmonicParams::default();
    p.bypassed = true;
    let out = run(&mut c, &x, &p);
    let max_delta = out.iter().zip(&x).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    assert!(max_delta < 1e-9, "bypass altered the signal: max delta {max_delta}");
}

#[test]
fn amount_zero_is_unity() {
    // Amount 0 → effective depth 0 → output == input (mix=1, output 0 dB).
    let n = 12_000usize;
    let x = drone(n, 90.0, 0.05);
    let mut c = HarmonicCleaner::new(SR);
    let p = HarmonicParams { amount: 0.0, ..HarmonicParams::default() };
    let out = run(&mut c, &x, &p);
    let tail_in = &x[6000..];
    let tail_out = &out[6000..];
    let max_delta = tail_out.iter().zip(tail_in).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    assert!(max_delta < 1e-4, "Amount 0 should be unity: max delta {max_delta}");
}

#[test]
fn mono_and_empty_out_r_is_safe() {
    // The plugin's mono path calls process_stereo with an empty out_r slice.
    let n = 6_000usize;
    let x = drone(n, 100.0, 0.05);
    let mut c = HarmonicCleaner::new(SR);
    let mut out_l = vec![0.0f32; n];
    let p = HarmonicParams::default();
    c.process_stereo(&x, &x, &mut out_l, &mut [], &p);
    assert!(out_l.iter().all(|v| v.is_finite()));
    assert!(rms(&out_l[3000..]) > 1e-4);
}
