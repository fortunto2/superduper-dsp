//! Regression test for the SC HPF bug fixed 2026-05-17.
//!
//! Symptom: user reported the GR meter stuck at -0.0 dB on a normal vocal
//! signal with SC HPF at 150 Hz. Cause: the HPF was applied AFTER
//! rectification, which subtracts the slow mean of `|x|` and crushes
//! the detected envelope by 6-10 dB — the detector then sat below the
//! threshold and the compressor never engaged.
//!
//! Fix: apply the single-pole high-pass to the raw signed audio, then
//! rectify. Standard sidechain topology (SSL G-style, Pro-C, Renaissance).

use superduper_synth_core::dsp_blocks::{
    compressor_gain_db, EnvelopeDetector,
};

const SR: f32 = 48000.0;

/// Mirror of the post-fix SC HPF: single-pole HP on the raw signed
/// audio. Same math the plugin uses in `process_stereo_block`.
fn sc_hpf_step(signed: f32, lp_state: &mut f32, hp_hz: f32, sr: f32) -> f32 {
    let coef = (-core::f32::consts::TAU * hp_hz / sr).exp();
    *lp_state = signed * (1.0 - coef) + *lp_state * coef;
    signed - *lp_state
}

/// Generate a 1 kHz sine at the given peak amplitude (linear, not dB).
fn sine_block(peak: f32, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| peak * (i as f32 * core::f32::consts::TAU * 1000.0 / SR).sin())
        .collect()
}

#[test]
fn sc_hpf_off_detects_peak_correctly() {
    // -6 dBFS sine. Peak = 0.5 linear, env should track ~0.5.
    let peak_lin = 0.5_f32;
    let buf = sine_block(peak_lin, (SR * 0.5) as usize);

    let mut det = EnvelopeDetector::default();
    let mut env = 0.0;
    for &x in &buf {
        env = det.process(x.abs(), SR, 5.0, 80.0);
    }
    let env_db = 20.0 * env.log10();
    println!("HPF off: env = {env:.4} ({env_db:.2} dB)");
    // Sine of peak 0.5 = -6 dBFS. Peak detector should land within ~1 dB.
    assert!(env_db > -7.5 && env_db < -5.0, "peak detect should be ~-6 dB, got {env_db}");
}

#[test]
fn sc_hpf_on_at_150hz_keeps_high_freq_envelope_intact() {
    // 1 kHz sine at -6 dBFS, SC HPF at 150 Hz. The HPF should pass nearly
    // all of a 1 kHz signal (cutoff is 7 octaves below), so the detected
    // envelope must stay close to -6 dB. If the filter regresses back to
    // the post-rectify topology this will collapse to ~-13 dB.
    let peak_lin = 0.5_f32;
    let buf = sine_block(peak_lin, (SR * 0.5) as usize);

    let mut det = EnvelopeDetector::default();
    let mut lp_state = 0.0_f32;
    let mut env = 0.0;
    for &x in &buf {
        let hp = sc_hpf_step(x, &mut lp_state, 150.0, SR);
        env = det.process(hp.abs(), SR, 5.0, 80.0);
    }
    let env_db = 20.0 * env.log10();
    println!("HPF on @150 Hz, 1 kHz sine: env = {env:.4} ({env_db:.2} dB)");
    assert!(
        env_db > -7.5 && env_db < -5.0,
        "150 Hz HPF must not crush a 1 kHz signal (got {env_db} dB, want ~-6)"
    );
}

#[test]
fn full_chain_threshold_minus_12_compresses_minus_6_dbfs_sine() {
    // The user's screenshot exactly: threshold -12, ratio 2:1, knee 12,
    // SC HPF 150 Hz, attack 30 ms, release 200 ms. A -6 dBFS sine sits
    // 6 dB above threshold; at 2:1 we expect ~3 dB of GR (slightly less
    // due to the 12 dB knee softening).
    let peak_lin = 0.5_f32;
    let buf = sine_block(peak_lin, (SR * 0.5) as usize);

    let mut det = EnvelopeDetector::default();
    let mut lp_state = 0.0_f32;
    let mut max_gr = 0.0_f32;
    for &x in &buf {
        let hp = sc_hpf_step(x, &mut lp_state, 150.0, SR);
        let env = det.process(hp.abs(), SR, 30.0, 200.0);
        let env_db = 20.0 * env.max(1e-9).log10();
        let gr = compressor_gain_db(env_db, -12.0, 2.0, 12.0);
        if gr < max_gr { max_gr = gr; }
    }
    println!("max GR end-to-end (post-fix) = {max_gr:.2} dB");
    assert!(
        max_gr < -1.5,
        "compressor must produce >1.5 dB GR (got {max_gr}) — the SC HPF bug is back"
    );
}

#[test]
fn sc_hpf_filters_subbass_as_intended() {
    // 60 Hz sine — sub bass that the SC HPF at 150 Hz is supposed to
    // attenuate so it doesn't pump the compressor. With the filter on
    // we should see the detected envelope drop noticeably below the
    // unfiltered reference.
    let peak_lin = 0.5_f32;
    let buf: Vec<f32> = (0..(SR * 0.5) as usize)
        .map(|i| peak_lin * (i as f32 * core::f32::consts::TAU * 60.0 / SR).sin())
        .collect();

    let mut det_off = EnvelopeDetector::default();
    let mut env_off = 0.0;
    for &x in &buf {
        env_off = det_off.process(x.abs(), SR, 5.0, 80.0);
    }
    let mut det_on = EnvelopeDetector::default();
    let mut lp_state = 0.0_f32;
    let mut env_on = 0.0;
    for &x in &buf {
        let hp = sc_hpf_step(x, &mut lp_state, 150.0, SR);
        env_on = det_on.process(hp.abs(), SR, 5.0, 80.0);
    }
    let drop_db = 20.0 * (env_off / env_on.max(1e-9)).log10();
    println!("60 Hz sine: off→{env_off:.3}, on→{env_on:.3}, attenuation = {drop_db:.2} dB");
    assert!(
        drop_db > 3.0,
        "SC HPF should attenuate 60 Hz below 150 Hz cutoff (got {drop_db} dB)"
    );
}
