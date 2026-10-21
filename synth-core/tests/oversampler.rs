//! Oversampler must be transparent in the audio band and report its exact
//! delay. Until 2026-10-20 it was neither: an 11-tap filter wrapped over a
//! 6-deep ring turned "2× oversampling" into a ±8 dB EQ (−17.8 dB at
//! 15 kHz), and it was the Saturator's default.

use superduper_synth_core::dsp_blocks::{oversample_apply, Oversampler2x, OVERSAMPLE_LATENCY};

const SR: f64 = 44_100.0;

fn impulse_response(os_mode: u32, len: usize) -> Vec<f32> {
    let mut os1 = Oversampler2x::default();
    let mut os2 = Oversampler2x::default();
    (0..len)
        .map(|n| oversample_apply(if n == 0 { 1.0 } else { 0.0 }, os_mode, &mut os1, &mut os2, |s| s))
        .collect()
}

fn gain_db(ir: &[f32], hz: f64) -> f64 {
    let w = 2.0 * std::f64::consts::PI * hz / SR;
    let (re, im) = ir.iter().enumerate().fold((0.0, 0.0), |(re, im), (n, &h)| {
        (re + h as f64 * (w * n as f64).cos(), im - h as f64 * (w * n as f64).sin())
    });
    20.0 * (re * re + im * im).sqrt().log10()
}

#[test]
fn latency_matches_the_published_constant() {
    for mode in 0..3u32 {
        let ir = impulse_response(mode, 256);
        let peak = ir
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .map(|(i, _)| i)
            .unwrap();
        assert_eq!(peak, OVERSAMPLE_LATENCY[mode as usize], "OS mode {mode}: impulse peak");
    }
}

#[test]
fn identity_is_flat_through_the_audio_band() {
    let freqs = [20.0, 100.0, 1_000.0, 4_000.0, 8_000.0, 12_000.0, 15_000.0, 18_000.0];
    for mode in 1..3u32 {
        let ir = impulse_response(mode, 512);
        let line: Vec<String> = freqs.iter().map(|&f| format!("{:>6.0}:{:+.3}", f, gain_db(&ir, f))).collect();
        println!("OS mode {mode}: {}", line.join("  "));
        for &f in &freqs {
            let g = gain_db(&ir, f);
            assert!(g.abs() < 0.05, "OS mode {mode}: {g:+.3} dB at {f} Hz (must be flat ±0.05 dB to 18 kHz)");
        }
    }
}

#[test]
fn identity_with_matched_dry_does_not_comb() {
    // A 50/50 dry/wet mix with the dry delayed by the published latency must
    // stay flat — the Saturator's Mix knob relies on this.
    for mode in 1..3u32 {
        let lat = OVERSAMPLE_LATENCY[mode as usize];
        let wet = impulse_response(mode, 512);
        let mixed: Vec<f32> = wet
            .iter()
            .enumerate()
            .map(|(n, &w)| 0.5 * w + 0.5 * if n == lat { 1.0 } else { 0.0 })
            .collect();
        for f in [1_000.0, 6_000.0, 12_000.0, 16_000.0] {
            let g = gain_db(&mixed, f);
            assert!(g.abs() < 0.05, "OS mode {mode} 50/50 mix: {g:+.3} dB at {f} Hz");
        }
    }
}
