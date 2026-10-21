//! Known-answer tests for the Dual (RMS + peak) detector and the bus key.
//!
//! The detector is driven directly (`superduper_compressor::detector`), the
//! same struct `process_stereo_block` calls per sample. The whole plugin with
//! Detector = Dual is pinned by the `dual_*` probes in tests/quality.rs.

use std::sync::atomic::Ordering;
use superduper_compressor::detector::{blend_key, key_level_db, DetCfg, DetOut, Detector, KEY_CREST_DB};
use superduper_synth_core::dsp_blocks::CompressorCurve;
use superduper_synth_core::gui::LiveScope;

const SR: f32 = 48_000.0;

fn cfg(dual: bool) -> DetCfg {
    DetCfg {
        threshold: -18.0, ratio: 4.0, knee: 0.0, curve: CompressorCurve::Clean,
        attack_ms: 1.0, release_ms: 120.0,
        dual, crest_db: 9.0, release_peak_ms: 40.0, release_rms_ms: 400.0,
    }
}

fn sine(amp: f32, i: usize) -> f32 { amp * (core::f32::consts::TAU * 1000.0 * i as f32 / SR).sin() }

/// Run `n` samples of `f(i)` (rectified, like the plugin's detector input)
/// and return every output.
fn run(det: &mut Detector, c: &DetCfg, n: usize, f: impl Fn(usize) -> f32) -> Vec<DetOut> {
    (0..n).map(|i| det.process(f(i).abs(), c, SR)).collect()
}

#[test]
fn dual_steady_sine_is_the_rms_stage_alone() {
    // 0.5 amplitude sine: RMS −9.03 dBFS, 8.97 dB over −18 at 4:1, hard knee
    // → RMS-stage GR = −8.97 · 0.75 = −6.73 dB. Peak after it −6.02 − 6.73
    // = −12.75, under −18 + 9 = −9 → peak stage idle.
    let c = cfg(true);
    let mut det = Detector::default();
    let out = run(&mut det, &c, SR as usize, |i| sine(0.5, i));
    let tail = &out[out.len() - 4800..];
    let expected = -(20.0 * (0.5f32 / 2f32.sqrt()).log10() + 18.0) * 0.75;
    for o in tail {
        assert!((o.rms_gr_db - expected).abs() < 0.15, "rms stage {} vs {expected}", o.rms_gr_db);
        assert_eq!(o.peak_gr_db, 0.0, "peak stage must stay idle on a steady tone");
        assert_eq!(o.gr_db, o.rms_gr_db);
    }
}

#[test]
fn dual_peak_stage_catches_the_spike_only() {
    // 0.25 sine (RMS −15 dBFS → ~−2.25 dB program GR), then a 3 ms burst at
    // full scale. The peak stage must act during the burst, be idle before
    // it, and let go again after Release Peak.
    let c = cfg(true);
    let mut det = Detector::default();
    let (start, len) = (SR as usize, (0.003 * SR) as usize);
    let sig = |i: usize| if (start..start + len).contains(&i) { sine(1.0, i) } else { sine(0.25, i) };
    let out = run(&mut det, &c, 2 * SR as usize, sig);

    let before = &out[start - 4800..start];
    assert!(before.iter().all(|o| o.peak_gr_db == 0.0), "peak stage idle before the spike");
    let rms_before = before.last().unwrap().rms_gr_db;
    assert!((rms_before + 2.25).abs() < 0.2, "program GR before the spike {rms_before}");

    let spike_peak = out[start..start + len + 48].iter().map(|o| o.peak_gr_db).fold(0.0f32, f32::min);
    assert!(spike_peak < -3.0, "peak stage should catch the spike, got {spike_peak}");
    // The RMS stage barely moves on 3 ms: that is the point of the split.
    let spike_rms = out[start..start + len + 48].iter().map(|o| o.rms_gr_db).fold(0.0f32, f32::min);
    assert!(spike_rms > rms_before - 1.5, "rms stage overreacted to a 3 ms spike: {spike_rms}");

    // 0.5 s later (> 10× Release Peak) the peak stage is idle again.
    let after = &out[start + SR as usize / 2..start + SR as usize / 2 + 4800];
    assert!(after.iter().all(|o| o.peak_gr_db > -0.01), "peak stage released");
}

#[test]
fn classic_is_untouched_by_the_dual_settings() {
    // Classic ignores crest / release peak / release RMS entirely.
    let (a, mut b) = (cfg(false), cfg(false));
    b.crest_db = 0.0;
    b.release_peak_ms = 5.0;
    b.release_rms_ms = 3000.0;
    let (mut da, mut db) = (Detector::default(), Detector::default());
    let oa = run(&mut da, &a, 9600, |i| sine(0.5, i));
    let ob = run(&mut db, &b, 9600, |i| sine(0.5, i));
    assert_eq!(oa, ob);
    assert!(oa.iter().all(|o| o.peak_gr_db == 0.0));
}

fn keyed_scope(band_db: f32) -> LiveScope {
    let sc = LiveScope::new(64);
    let e = superduper_dsp_sdk::bus::band_edges();
    for (b, a) in sc.key_bands.iter().enumerate() {
        let centre = (e[b] * e[b + 1]).sqrt();
        // Loud between 300 Hz and 3 kHz, silent elsewhere.
        a.store(if (300.0..3000.0).contains(&centre) { band_db } else { -120.0 }, Ordering::Relaxed);
    }
    sc
}

#[test]
fn key_level_is_the_band_limited_rms() {
    // Bands add in power: n bands at −20 dB each read −20 + 10·log10(n)
    // over a range that covers just them. Between band centres the sampled
    // points interpolate, so allow a dB.
    let sc = keyed_scope(-20.0);
    let e = superduper_dsp_sdk::bus::band_edges();
    let n = (0..32).filter(|&b| (300.0..3000.0).contains(&(e[b] * e[b + 1]).sqrt())).count() as f32;
    let got = key_level_db(&sc, 300.0, 3000.0);
    assert!((got - (-20.0 + 10.0 * n.log10())).abs() < 1.5, "key level {got}, bands {n}");
    // A range where the key is silent reads silent.
    assert!(key_level_db(&sc, 6000.0, 16000.0) < -100.0);
    // An unset key (all −120) reads silent too.
    assert!(key_level_db(&LiveScope::new(64), 100.0, 5000.0) < -100.0);
}

#[test]
fn key_blend_one_compresses_a_quiet_input_on_a_loud_key() {
    // Input −40 dBFS (far under −18): Classic alone does nothing. With the
    // key loud in Key Lo..Hi and Key = 1, the detector hears the key.
    let sc = keyed_scope(-20.0);
    let key_db = key_level_db(&sc, 300.0, 3000.0);
    let key_lin = 10f32.powf((key_db + KEY_CREST_DB) / 20.0);
    let c = cfg(false);
    let quiet = |i: usize| sine(0.01, i);

    let mut det = Detector::default();
    let off: Vec<_> = (0..SR as usize).map(|i| det.process(blend_key(quiet(i).abs(), key_lin, 0.0), &c, SR)).collect();
    assert!(off.iter().all(|o| o.gr_db == 0.0), "key 0 = input only");

    let mut det = Detector::default();
    let on: Vec<_> = (0..SR as usize).map(|i| det.process(blend_key(quiet(i).abs(), key_lin, 1.0), &c, SR)).collect();
    let gr = on.last().unwrap().gr_db;
    let expected = -((key_db + KEY_CREST_DB) + 18.0) * 0.75;
    assert!((gr - expected).abs() < 0.3, "keyed GR {gr} vs {expected} (key {key_db} dB)");

    // Dual hears the key too (both stages share the blended input).
    let mut det = Detector::default();
    let d = cfg(true);
    let on: Vec<_> = (0..SR as usize).map(|i| det.process(blend_key(quiet(i).abs(), key_lin, 1.0), &d, SR)).collect();
    assert!(on.last().unwrap().rms_gr_db < -3.0);

    // Key off (silent track) with Key = 1 releases: no GR on a quiet input.
    let silent = 10f32.powf((key_level_db(&LiveScope::new(64), 300.0, 3000.0) + KEY_CREST_DB) / 20.0);
    let mut det = Detector::default();
    let out: Vec<_> = (0..4800).map(|i| det.process(blend_key(quiet(i).abs(), silent, 1.0), &c, SR)).collect();
    assert!(out.iter().all(|o| o.gr_db == 0.0));
}

#[test]
fn blend_key_zero_is_bit_exact() {
    for x in [0.0f32, 1e-7, 0.3, 0.999] {
        assert_eq!(blend_key(x, 0.7, 0.0).to_bits(), x.to_bits());
    }
}
