//! Quality test — measure the two things that actually matter for the
//! harmonic-comb denoiser on a synthetic kubyz-like signal:
//!   (a) NOISE REDUCTION — between-harmonic / inharmonic energy drops as Amount
//!       goes up.
//!   (b) TRANSIENT PRESERVATION — a pluck onset's sharpness survives (the comb
//!       re-opens on attacks) and Transient=high keeps it sharper than
//!       Transient=low.
//!
//! Run: `cargo test -p superduper-harmonic --test denoise -- --nocapture`

use superduper_synth_core::analysis::spectrum_with_freq;
use superduper_harmonic::dsp::{HarmonicCleaner, HarmonicParams, MODE_MEAN, MODE_MEDIAN};

const SR: f32 = 48_000.0;
const F0: f32 = 90.0;

/// A kubyz-like signal: a steady harmonic drone at F0, plus low-level broadband
/// inharmonic noise (the contact rustle), plus sharp broadband plucks at fixed
/// times (the jaw-harp attacks). Deterministic.
struct Scene {
    x: Vec<f32>,
    pluck_samples: Vec<usize>,
}

fn kubyz_scene(n: usize, noise_amp: f32) -> Scene {
    use std::f32::consts::TAU;
    let mut s: u32 = 0x2468_ACE0;
    let pluck_times = [0.5f32, 1.0, 1.5];
    let pluck_samples: Vec<usize> = pluck_times.iter().map(|&t| (t * SR) as usize).collect();
    let x = (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            // Steady harmonic drone.
            let mut v = 0.0;
            for h in 1..=14 {
                v += (0.5 / h as f32) * (TAU * F0 * h as f32 * t).sin();
            }
            let mut sig = v * 0.22;
            // Broadband inharmonic noise (the contact rustle we want to reject).
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let noise = (s as f32 / u32::MAX as f32) * 2.0 - 1.0;
            sig += noise * noise_amp;
            // Sharp broadband plucks: fast attack, ~8 ms exp decay.
            for &ps in &pluck_samples {
                if i >= ps {
                    let dt = (i - ps) as f32 / SR;
                    if dt < 0.05 {
                        let env = (-dt / 0.008).exp();
                        // Deterministic broadband burst (a short noisy click).
                        s ^= s << 13;
                        s ^= s >> 17;
                        s ^= s << 5;
                        let burst = (s as f32 / u32::MAX as f32) * 2.0 - 1.0;
                        sig += 0.7 * env * burst;
                    }
                }
            }
            sig
        })
        .collect();
    Scene { x, pluck_samples }
}

fn run(x: &[f32], p: &HarmonicParams) -> Vec<f32> {
    let n = x.len();
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let mut c = HarmonicCleaner::new(SR);
    c.process_stereo(x, x, &mut out_l, &mut out_r, p);
    out_l
}

fn lin_spectrum(x: &[f32]) -> Vec<(f32, f32)> {
    spectrum_with_freq(x, SR)
        .into_iter()
        .map(|(hz, db)| (hz, 10f32.powf(db / 20.0)))
        .collect()
}

/// Energy in `[lo, hi]` at frequencies that are NOT within `tol` Hz of a
/// harmonic of `f0` — i.e. the between-harmonic noise floor.
fn between_harmonic_energy(spec: &[(f32, f32)], f0: f32, lo: f32, hi: f32, tol: f32) -> f32 {
    spec.iter()
        .filter(|(hz, _)| *hz >= lo && *hz <= hi)
        .filter(|(hz, _)| {
            let k = (hz / f0).round().max(1.0);
            (hz - k * f0).abs() > tol
        })
        .map(|(_, m)| m * m)
        .sum()
}

fn energy_db(e: f32) -> f32 {
    10.0 * e.max(1e-30).log10()
}

fn window_peak(x: &[f32], start: usize, len: usize) -> f32 {
    let end = (start + len).min(x.len());
    x[start..end].iter().map(|v| v.abs()).fold(0.0, f32::max)
}

#[test]
fn noise_between_harmonics_drops_with_amount() {
    let n = (SR * 2.0) as usize;
    let scene = kubyz_scene(n, 0.05);

    let dry = HarmonicParams { amount: 0.0, ..HarmonicParams::default() };
    let wet = HarmonicParams {
        amount: 0.9,
        bandwidth: 0.3, // narrow-ish → aggressive
        transient: 0.6,
        ..HarmonicParams::default()
    };
    let out_dry = run(&scene.x, &dry);
    let out_wet = run(&scene.x, &wet);

    // Analyse a STEADY segment well away from any pluck (1.7–1.95 s).
    let a = (1.7 * SR) as usize;
    let b = (1.95 * SR) as usize;
    let sd = lin_spectrum(&out_dry[a..b]);
    let sw = lin_spectrum(&out_wet[a..b]);

    // Between-harmonic band 400–5000 Hz, excluding ±25 Hz around each harmonic.
    let e_dry = between_harmonic_energy(&sd, F0, 400.0, 5000.0, 25.0);
    let e_wet = between_harmonic_energy(&sw, F0, 400.0, 5000.0, 25.0);
    let drop_db = energy_db(e_dry) - energy_db(e_wet);
    println!(
        "between-harmonic noise: dry {:.1} dB, wet {:.1} dB → reduction {:.1} dB",
        energy_db(e_dry),
        energy_db(e_wet),
        drop_db
    );
    assert!(
        drop_db >= 3.0,
        "comb did not reduce the between-harmonic noise enough: {drop_db:.1} dB (want ≥ 3)"
    );
    assert!(out_wet.iter().all(|v| v.is_finite()), "non-finite output");
}

#[test]
fn harmonic_content_is_preserved() {
    // Guard: the comb must keep the harmonics (unity at k·f0), not chew them.
    let n = (SR * 2.0) as usize;
    let scene = kubyz_scene(n, 0.05);
    let dry = HarmonicParams { amount: 0.0, ..HarmonicParams::default() };
    let wet = HarmonicParams { amount: 0.9, bandwidth: 0.3, ..HarmonicParams::default() };
    let out_dry = run(&scene.x, &dry);
    let out_wet = run(&scene.x, &wet);

    let a = (1.7 * SR) as usize;
    let b = (1.95 * SR) as usize;
    let sd = lin_spectrum(&out_dry[a..b]);
    let sw = lin_spectrum(&out_wet[a..b]);

    // Energy AT the harmonics (±25 Hz) should be essentially untouched.
    let harm = |spec: &[(f32, f32)]| -> f32 {
        spec.iter()
            .filter(|(hz, _)| *hz >= 60.0 && *hz <= 5000.0)
            .filter(|(hz, _)| {
                let k = (hz / F0).round().max(1.0);
                (hz - k * F0).abs() <= 25.0
            })
            .map(|(_, m)| m * m)
            .sum()
    };
    let hd = energy_db(harm(&sd));
    let hw = energy_db(harm(&sw));
    println!("harmonic energy: dry {hd:.1} dB, wet {hw:.1} dB (Δ {:.1} dB)", hw - hd);
    assert!(
        (hw - hd).abs() <= 2.0,
        "comb altered the harmonic content too much: dry {hd:.1} vs wet {hw:.1} dB"
    );
}

/// A transient-HEAVY scene at a real kubyz f0 (73 Hz): a steady drone + a LOW
/// stationary noise floor + FREQUENT sharp clicks (every 120 ms). This is the
/// user's actual material — piezo contact rustle is transient. It's the case
/// that exposed the mean comb's pluck-echo (it re-injects each click at T, 2T…
/// which, being inharmonic, ADDS between-harmonic energy).
fn transient_heavy_scene(n: usize) -> Vec<f32> {
    use std::f32::consts::TAU;
    let f0 = 73.0f32;
    let mut s: u32 = 0x1111_2222;
    let click_dt = 0.12f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let mut v = 0.0;
            for h in 1..=14 {
                v += (0.5 / h as f32) * (TAU * f0 * h as f32 * t).sin();
            }
            let mut sig = v * 0.22;
            // Low stationary noise floor.
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            sig += ((s as f32 / u32::MAX as f32) * 2.0 - 1.0) * 0.015;
            // Frequent sharp clicks from 0.2 s on.
            if t >= 0.2 {
                let phase = ((t - 0.2) / click_dt).fract() * click_dt;
                if phase < 0.05 {
                    let env = (-phase / 0.008).exp();
                    s ^= s << 13;
                    s ^= s >> 17;
                    s ^= s << 5;
                    let burst = (s as f32 / u32::MAX as f32) * 2.0 - 1.0;
                    sig += 0.6 * env * burst;
                }
            }
            sig
        })
        .collect()
}

/// A perfectly periodic drone (no noise, no click) — the reference the comb
/// should pass at unity.
fn periodic_drone(n: usize, f0: f32) -> Vec<f32> {
    use std::f32::consts::TAU;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let mut v = 0.0;
            for h in 1..=14 {
                v += (0.5 / h as f32) * (TAU * f0 * h as f32 * t).sin();
            }
            v * 0.22
        })
        .collect()
}

#[test]
fn median_reduces_noise_on_transient_material() {
    // On transient-heavy material (the user's real case) the median comb must
    // still REDUCE the between-harmonic noise (not raise it), and be at least as
    // good as the mean comb. (The mean's pluck-echo is isolated cleanly in the
    // next test; here we just require median to keep cleaning on clicky input.)
    let f0 = 73.0f32;
    let n = (SR * 2.0) as usize;
    let x = transient_heavy_scene(n);

    let dry = HarmonicParams { amount: 0.0, ..HarmonicParams::default() };
    let median = HarmonicParams { amount: 0.9, bandwidth: 0.3, transient: 0.6, mode: MODE_MEDIAN, ..HarmonicParams::default() };
    let mean = HarmonicParams { mode: MODE_MEAN, ..median };

    let a = (0.3 * SR) as usize;
    let b = (1.9 * SR) as usize;
    let e_dry = energy_db(between_harmonic_energy(&lin_spectrum(&run(&x, &dry)[a..b]), f0, 400.0, 5000.0, 15.0));
    let e_med = energy_db(between_harmonic_energy(&lin_spectrum(&run(&x, &median)[a..b]), f0, 400.0, 5000.0, 15.0));
    let e_mean = energy_db(between_harmonic_energy(&lin_spectrum(&run(&x, &mean)[a..b]), f0, 400.0, 5000.0, 15.0));
    println!(
        "transient-heavy between-harmonic: dry {e_dry:.1} dB | MEAN {e_mean:.1} dB ({:+.1} vs dry) | MEDIAN {e_med:.1} dB ({:+.1} vs dry)",
        e_mean - e_dry,
        e_med - e_dry
    );
    assert!(
        e_med < e_dry - 1.0,
        "median failed to reduce between-harmonic noise on transient material: {e_med:.1} vs dry {e_dry:.1} dB"
    );
    assert!(
        e_med <= e_mean + 0.3,
        "median should be at least as clean as mean here: median {e_med:.1} vs mean {e_mean:.1} dB"
    );
}

#[test]
fn median_does_not_echo_a_single_pluck() {
    // THE fix, isolated. One sharp broadband pluck on a perfectly periodic drone,
    // no stationary noise. With Transient=0 (comb fully active — the "echo comes
    // later, no onset to protect it" worst case), the MEAN comb re-injects the
    // pluck at T, 2T, … (each ≈ pluck/K); the MEDIAN comb discards that single
    // outlier tap → no echo. We measure the residual (out − drone-through-comb)
    // energy exactly at the echo positions t0+k·T, skipping the pluck itself.
    let f0 = 73.0f32;
    let t_samp = SR / f0;
    let n = (SR * 1.6) as usize;
    let click_at = (1.0 * SR) as usize;

    let drone = periodic_drone(n, f0);
    let mut sig = drone.clone();
    let mut s: u32 = 0x9E37_79B9;
    for j in 0..(0.03 * SR) as usize {
        let idx = click_at + j;
        if idx < n {
            let env = (-(j as f32 / SR) / 0.0025).exp();
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let burst = (s as f32 / u32::MAX as f32) * 2.0 - 1.0;
            sig[idx] += 0.7 * env * burst;
        }
    }

    // Residual energy at echo positions t0 + k·T (k = 1..6), ±0.12·T windows.
    let echo_energy = |mode: u32| -> f32 {
        let p = HarmonicParams {
            amount: 1.0,
            bandwidth: 0.4, // K = 6
            transient: 0.0, // comb fully active — worst case for echo
            mode,
            ..HarmonicParams::default()
        };
        let out = run(&sig, &p);
        let base = run(&drone, &p);
        let w = (0.12 * t_samp) as usize;
        let mut e = 0.0f32;
        for k in 1..6 {
            let c = click_at + (k as f32 * t_samp) as usize;
            for i in c.saturating_sub(w)..(c + w).min(n) {
                let r = out[i] - base[i];
                e += r * r;
            }
        }
        e
    };

    let em_mean = echo_energy(MODE_MEAN);
    let em_med = echo_energy(MODE_MEDIAN);
    let rel_db = 10.0 * (em_med.max(1e-30) / em_mean.max(1e-30)).log10();
    println!(
        "single-pluck echo energy: MEAN {em_mean:.5}, MEDIAN {em_med:.5} → median {rel_db:.1} dB vs mean"
    );
    assert!(em_mean > 1e-6, "test setup: mean should produce a measurable echo, got {em_mean:.6}");
    assert!(
        em_med < 0.35 * em_mean,
        "median failed to reject the pluck echo: median {em_med:.5} vs mean {em_mean:.5} ({rel_db:.1} dB)"
    );
}

#[test]
fn pluck_transient_survives_and_transient_control_works() {
    let n = (SR * 2.0) as usize;
    let scene = kubyz_scene(n, 0.05);

    let dry = HarmonicParams { amount: 0.0, ..HarmonicParams::default() };
    // Aggressive comb, but transient preservation ON.
    let t_hi = HarmonicParams { amount: 0.9, bandwidth: 0.3, transient: 1.0, ..HarmonicParams::default() };
    // Same comb, transient preservation OFF (worst-case smearing).
    let t_lo = HarmonicParams { amount: 0.9, bandwidth: 0.3, transient: 0.0, ..HarmonicParams::default() };

    let out_dry = run(&scene.x, &dry);
    let out_thi = run(&scene.x, &t_hi);
    let out_tlo = run(&scene.x, &t_lo);

    // Look at the last pluck (1.5 s) — the comb history is fully warmed up.
    let ps = scene.pluck_samples[2];
    let len = (0.02 * SR) as usize; // 20 ms attack window
    let pk_dry = window_peak(&out_dry, ps, len);
    let pk_thi = window_peak(&out_thi, ps, len);
    let pk_tlo = window_peak(&out_tlo, ps, len);
    println!(
        "pluck peak: dry {pk_dry:.3} | transient=1 {pk_thi:.3} ({:.0}%) | transient=0 {pk_tlo:.3} ({:.0}%)",
        100.0 * pk_thi / pk_dry,
        100.0 * pk_tlo / pk_dry
    );

    // With transient preservation on, the pluck must survive nearly intact.
    assert!(
        pk_thi >= 0.7 * pk_dry,
        "pluck smeared even with Transient=1: {pk_thi:.3} vs dry {pk_dry:.3}"
    );
    // And Transient=1 must keep the pluck sharper than Transient=0 (the control
    // actually does something).
    assert!(
        pk_thi > pk_tlo + 1e-4,
        "Transient control has no effect: t=1 {pk_thi:.3} vs t=0 {pk_tlo:.3}"
    );
}
