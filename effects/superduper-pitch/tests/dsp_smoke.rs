//! Stability smoke tests for the pitch shifter DSP.
//!
//! Run: `cargo test -p superduper-pitch --test dsp_smoke -- --nocapture`

use superduper_pitch::dsp::{PitchParams, PitchShifter};

const SR: f32 = 48_000.0;

fn voice(f0: f32, n: usize) -> Vec<f32> {
    use std::f32::consts::TAU;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let mut s = 0.0;
            for h in 1..=8 {
                s += (1.0 / h as f32) * (TAU * f0 * h as f32 * t).sin();
            }
            s * 0.15
        })
        .collect()
}

fn params(pitch: f32, formant: f32) -> PitchParams {
    PitchParams { pitch_st: pitch, formant_st: formant, mix: 1.0, output_lin: 1.0, bypassed: false }
}

fn run(m: &[f32], p: &PitchParams) -> (Vec<f32>, Vec<f32>) {
    let n = m.len();
    let mut ol = vec![0.0f32; n];
    let mut or = vec![0.0f32; n];
    let mut sh = PitchShifter::new(SR, 512);
    let mut i = 0;
    while i < n {
        let end = (i + 512).min(n);
        let inb = &m[i..end];
        let mut bl = vec![0.0f32; end - i];
        let mut br = vec![0.0f32; end - i];
        sh.process(inb, inb, &mut bl, &mut br, p);
        ol[i..end].copy_from_slice(&bl);
        or[i..end].copy_from_slice(&br);
        i = end;
    }
    (ol, or)
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|&v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}
fn finite(x: &[f32]) -> bool {
    x.iter().all(|v| v.is_finite())
}

#[test]
fn shifting_is_stable_and_audible() {
    let m = voice(150.0, (SR * 2.0) as usize);
    for (p, f) in [(7.0, 0.0), (-7.0, 0.0), (0.0, 6.0), (12.0, -4.0), (-12.0, 5.0)] {
        let (ol, _) = run(&m, &params(p, f));
        assert!(finite(&ol), "pitch={p} formant={f}: non-finite output");
        let r = rms(&ol[ol.len() / 2..]);
        let peak = ol.iter().map(|v| v.abs()).fold(0.0, f32::max);
        println!("pitch={p:+} formant={f:+}: rms={r:.4} peak={peak:.4}");
        assert!(r > 1e-3, "pitch={p} formant={f}: output too quiet ({r})");
        assert!(peak < 8.0, "pitch={p} formant={f}: output blew up ({peak})");
    }
}

#[test]
fn silence_stays_finite_and_quiet() {
    let m = vec![0.0f32; (SR * 1.0) as usize];
    let (ol, _) = run(&m, &params(12.0, 6.0));
    assert!(finite(&ol), "silence produced non-finite output");
    assert!(rms(&ol) < 1e-3, "silence produced audible output");
}

#[test]
fn all_presets_in_range() {
    use superduper_pitch::presets::PRESETS;
    use superduper_pitch::PARAMS;
    for preset in PRESETS {
        assert_eq!(preset.values.len(), PARAMS.len(), "preset '{}' wrong length", preset.name);
        for (i, &v) in preset.values.iter().enumerate() {
            let def = &PARAMS[i];
            assert!(
                v >= def.min as f32 - 1e-4 && v <= def.max as f32 + 1e-4,
                "preset '{}' param {} = {} out of range [{}, {}]",
                preset.name, i, v, def.min, def.max
            );
        }
    }
    println!("{} presets validated", PRESETS.len());
}

#[test]
fn bypass_passes_through() {
    let m = voice(150.0, 4096);
    let (ol, _) = run(&m, &PitchParams { pitch_st: 7.0, formant_st: 0.0, mix: 1.0, output_lin: 1.0, bypassed: true });
    for i in 0..m.len() {
        assert!((ol[i] - m[i]).abs() < 1e-6, "bypass altered sample {i}");
    }
}

#[test]
fn dry_wet_mix_blends() {
    // Mix = 0 should return the (latency-delayed) dry signal → same pitch as in.
    let m = voice(150.0, (SR * 2.0) as usize);
    let (ol, _) = run(&m, &PitchParams { pitch_st: 12.0, formant_st: 0.0, mix: 0.0, output_lin: 1.0, bypassed: false });
    assert!(finite(&ol));
    // Dry-only output energy should roughly match the input energy.
    let r_in = rms(&m[m.len() / 2..]);
    let r_out = rms(&ol[ol.len() / 2..]);
    println!("mix=0 dry: in rms {r_in:.4} → out rms {r_out:.4}");
    assert!(r_out > r_in * 0.5 && r_out < r_in * 1.5, "mix=0 should pass dry (in {r_in:.4}, out {r_out:.4})");
}
