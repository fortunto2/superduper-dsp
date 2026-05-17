//! DSP smoke tests for SuperDuper EQ — verify the shared Biquad block
//! behaves per RBJ spec. Critical test: peaking EQ with boost+cut at the
//! same f/Q must produce unity response (cookbook guarantee).

use superduper_synth_core::dsp_blocks::Biquad;

const SR: f32 = 48_000.0;

fn rms(s: &[f32]) -> f32 {
    let n = s.len() as f32;
    (s.iter().map(|x| x * x).sum::<f32>() / n).sqrt()
}

#[test]
fn peaking_unity_when_boost_followed_by_cut() {
    // RBJ cookbook guarantees: peaking EQ at +N dB then -N dB with the same
    // freq and Q is exactly flat.
    let mut boost = Biquad::default();
    let mut cut = Biquad::default();
    boost.set_peaking(SR, 1000.0, 1.0, 6.0);
    cut.set_peaking(SR, 1000.0, 1.0, -6.0);

    let mut peak_diff = 0.0_f32;
    for i in 0..4096 {
        let phase = i as f32 * 2.0 * core::f32::consts::PI * 1500.0 / SR;
        let x = phase.sin();
        let y = cut.process(boost.process(x));
        if i > 200 {
            peak_diff = peak_diff.max((x - y).abs());
        }
    }
    println!("boost+cut peak diff: {peak_diff:.6}");
    assert!(peak_diff < 0.01, "boost+cut not flat (peak diff = {peak_diff})");
}

#[test]
fn low_shelf_boosts_lows_cuts_highs_zero() {
    // Low shelf at 200 Hz +6 dB. 1 kHz sine should pass through ≈ unity
    // (above the shelf). 50 Hz sine should be boosted.
    let mut high = Biquad::default();
    let mut low = Biquad::default();
    high.set_low_shelf(SR, 200.0, 1.0, 6.0);
    low.set_low_shelf(SR, 200.0, 1.0, 6.0);

    let n = 4096;
    let high_sine: Vec<f32> = (0..n).map(|i| {
        let p = i as f32 * 2.0 * core::f32::consts::PI * 2000.0 / SR;
        high.process(p.sin())
    }).collect();
    let low_sine: Vec<f32> = (0..n).map(|i| {
        let p = i as f32 * 2.0 * core::f32::consts::PI * 50.0 / SR;
        low.process(p.sin())
    }).collect();

    // Skip the transient.
    let high_rms = rms(&high_sine[500..]);
    let low_rms = rms(&low_sine[500..]);
    println!("low shelf +6 dB: high(2k)={high_rms:.3}, low(50)={low_rms:.3}");
    // Unit sine has RMS ~0.707. ±0.05 tolerance for filter ripple.
    assert!((high_rms - 0.707).abs() < 0.05, "high stayed near unity? got {high_rms}");
    assert!(low_rms > 1.0, "low band should boost above unity, got {low_rms}");
}

#[test]
fn hpf_kills_low_passes_high() {
    let mut hp_low = Biquad::default();
    let mut hp_high = Biquad::default();
    hp_low.set_hpf(SR, 1000.0, 0.707);
    hp_high.set_hpf(SR, 1000.0, 0.707);

    let low: Vec<f32> = (0..4096).map(|i| {
        let p = i as f32 * 2.0 * core::f32::consts::PI * 100.0 / SR;
        hp_low.process(p.sin())
    }).collect();
    let high: Vec<f32> = (0..4096).map(|i| {
        let p = i as f32 * 2.0 * core::f32::consts::PI * 5000.0 / SR;
        hp_high.process(p.sin())
    }).collect();
    let low_rms = rms(&low[500..]);
    let high_rms = rms(&high[500..]);
    println!("HPF@1k: low(100)={low_rms:.3}, high(5k)={high_rms:.3}");
    assert!(low_rms < 0.1, "HPF should drop 100 Hz hard, got {low_rms}");
    assert!((high_rms - 0.707).abs() < 0.05, "HPF shouldn't touch 5 kHz, got {high_rms}");
}

#[test]
fn biquad_stable_at_extreme_q() {
    // High Q peaking shouldn't blow up.
    let mut b = Biquad::default();
    b.set_peaking(SR, 2000.0, 6.0, 12.0);
    let mut peak = 0.0_f32;
    for i in 0..4096 {
        let phase = i as f32 * 2.0 * core::f32::consts::PI * 2000.0 / SR;
        let y = b.process(phase.sin());
        peak = peak.max(y.abs());
    }
    println!("peaking Q=6 +12 dB at resonance peak: {peak:.3}");
    assert!(peak.is_finite() && peak < 10.0, "biquad blew up (peak={peak})");
}
