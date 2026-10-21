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

/// A 0.5 sine: RMS −9.03 dBFS, 8.97 dB over −18 at 4:1, hard knee → the RMS
/// stage asks −6.73 dB.
const HALF_RMS_GR: f32 = -(-9.0309 + 18.0) * 0.75;

/// What the peak stage's envelope (attack 1 ms, Release Peak 40 ms) settles
/// to on a rectified sine of `amp`, in dB: a hair under the true peak, since
/// the follower rides the rectified ripple.
fn peak_env_db(amp: f32, c: &DetCfg) -> f32 {
    let mut e = superduper_synth_core::dsp_blocks::EnvelopeDetector::default();
    let mut v = 0.0;
    for i in 0..SR as usize {
        v = e.process(sine(amp, i).abs(), SR, c.attack_ms, c.release_peak_ms);
    }
    20.0 * v.log10()
}

#[test]
fn dual_steady_sine_is_controlled_by_the_rms_stage() {
    // The two stages run in parallel on the same input and the deeper one
    // takes control. A sine's crest (3 dB) is under Peak Crest 9, so the peak
    // stage asks for less than the RMS stage (−6.02 + 9 = 2.98 over its
    // threshold → −2.24) and the RMS stage wins.
    let c = cfg(true);
    let mut det = Detector::default();
    let out = run(&mut det, &c, SR as usize, |i| sine(0.5, i));
    let tail = &out[out.len() - 4800..];
    let peak_expected = -(peak_env_db(0.5, &c) + 18.0 - 9.0) * 0.75;
    for o in tail {
        assert!((o.rms_gr_db - HALF_RMS_GR).abs() < 0.15, "rms stage {} vs {HALF_RMS_GR}", o.rms_gr_db);
        assert!((o.peak_gr_db - peak_expected).abs() < 0.15, "peak stage {} vs {peak_expected}", o.peak_gr_db);
        assert!(o.peak_gr_db > o.rms_gr_db, "the peak stage asks for less than the RMS stage on a steady tone");
        assert_eq!(o.gr_db, o.rms_gr_db, "the RMS stage is in control");
    }
}

#[test]
fn dual_total_is_the_deeper_stage_not_the_sum() {
    // Peak Crest 0: both stages share one threshold. On the 0.5 sine the
    // peak stage (−6.02 + 18 = 11.98 over → −8.98) is deeper than the RMS
    // stage (−6.73) and takes control. Summing them gave −10.7 dB and put a
    // steady tone's RMS 1.7 dB *below* the threshold; with the deeper stage
    // in control the output peak sits where a 4:1 peak compressor leaves it.
    let mut c = cfg(true);
    c.crest_db = 0.0;
    let mut det = Detector::default();
    let out = run(&mut det, &c, SR as usize, |i| sine(0.5, i));
    let o = out.last().unwrap();
    let peak_expected = -(peak_env_db(0.5, &c) + 18.0) * 0.75;
    println!("crest 0, 0.5 sine: rms {:.2}, peak {:.2}, total {:.2}", o.rms_gr_db, o.peak_gr_db, o.gr_db);
    assert!((o.peak_gr_db - peak_expected).abs() < 0.15, "peak stage {} vs {peak_expected}", o.peak_gr_db);
    assert!((o.rms_gr_db - HALF_RMS_GR).abs() < 0.15, "rms stage {} vs {HALF_RMS_GR}", o.rms_gr_db);
    assert_eq!(o.gr_db, o.rms_gr_db.min(o.peak_gr_db), "total = the deeper stage");
    assert!(o.gr_db > -9.2, "total {} is deeper than either stage: the stages were summed", o.gr_db);
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
    let spike_total = out[start..start + len + 48].iter().map(|o| o.gr_db).fold(0.0f32, f32::min);
    assert!((spike_total - spike_peak).abs() < 1e-5, "during the spike the peak stage is in control");
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

#[test]
fn classic_to_dual_switch_seeds_the_rms_stage_from_the_live_envelope() {
    // Classic on the 0.5 sine, then Dual mid-tone: the RMS stage starts from
    // the level the peak envelope already holds (a sine's mean square is half
    // its peak squared), not from silence — no dip to GR 0 on the switch.
    let (classic, dual) = (cfg(false), cfg(true));
    let mut det = Detector::default();
    let half = SR as usize / 2;
    let _ = run(&mut det, &classic, half, |i| sine(0.5, i));
    let out = run(&mut det, &dual, half, |i| sine(0.5, i + half));
    let (first, settled) = (out[0].gr_db, out[out.len() - 1].gr_db);
    println!("Classic→Dual on a steady tone: first sample {first:.2} dB, settled {settled:.2} dB");
    assert!((settled - HALF_RMS_GR).abs() < 0.15, "settled {settled} vs {HALF_RMS_GR}");
    assert!((first - settled).abs() < 1.0, "the switch pumped: {first:.2} dB on the first sample, {settled:.2} settled");
}

#[test]
fn dual_to_classic_and_back_does_not_resume_a_stale_rms_stage() {
    // Dual compressing a loud tone, Classic over a silent stretch, then Dual
    // again on a tone far under threshold: the RMS stage must read that quiet
    // tone, not the −6.7 dB it held before the detour.
    let (classic, dual) = (cfg(false), cfg(true));
    let mut det = Detector::default();
    let half = SR as usize / 2;
    let loud = run(&mut det, &dual, half, |i| sine(0.5, i));
    assert!((loud.last().unwrap().rms_gr_db - HALF_RMS_GR).abs() < 0.15);
    let _ = run(&mut det, &classic, half, |_| 0.0);
    let back = run(&mut det, &dual, 480, |i| sine(0.01, i));
    let worst = back.iter().map(|o| o.rms_gr_db).fold(0.0f32, f32::min);
    println!("Dual→Classic→Dual on a −40 dBFS tone: deepest RMS-stage GR in the first 10 ms {worst:.2} dB");
    assert!(worst > -0.5, "stale RMS stage after the switch: {worst:.2} dB on a tone 25 dB under threshold");
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
    // The key is a band RMS. Classic's detector is peak-ish, so the plugin
    // lifts it by a sine's crest (3 dB) for Classic only; Dual's RMS stage
    // squares the detector input and reads the RMS as it is.
    let key_lin_classic = 10f32.powf((key_db + KEY_CREST_DB) / 20.0);
    let key_lin_dual = 10f32.powf(key_db / 20.0);
    let c = cfg(false);
    let quiet = |i: usize| sine(0.01, i);

    let mut det = Detector::default();
    let off: Vec<_> = (0..SR as usize).map(|i| det.process(blend_key(quiet(i).abs(), key_lin_classic, 0.0), &c, SR)).collect();
    assert!(off.iter().all(|o| o.gr_db == 0.0), "key 0 = input only");

    let mut det = Detector::default();
    let on: Vec<_> = (0..SR as usize).map(|i| det.process(blend_key(quiet(i).abs(), key_lin_classic, 1.0), &c, SR)).collect();
    let gr = on.last().unwrap().gr_db;
    let expected = -((key_db + KEY_CREST_DB) + 18.0) * 0.75;
    assert!((gr - expected).abs() < 0.3, "Classic keyed GR {gr} vs {expected} (key {key_db} dB, +3 dB lift)");

    // Dual's RMS stage reads the key without the lift: a constant detector
    // input of key_lin has a mean square of exactly key_lin², so the GR is
    // the curve at key_db itself. The peak stage (key −11 dB vs −18 + 9)
    // stays idle, so the RMS stage is the total.
    let mut det = Detector::default();
    let d = cfg(true);
    let on: Vec<_> = (0..SR as usize).map(|i| det.process(blend_key(quiet(i).abs(), key_lin_dual, 1.0), &d, SR)).collect();
    let o = on.last().unwrap();
    let expected_dual = -(key_db + 18.0) * 0.75;
    assert!((o.rms_gr_db - expected_dual).abs() < 0.1, "Dual RMS stage {} vs {expected_dual} (key {key_db} dB, no lift)", o.rms_gr_db);
    // With the lift it would be 2.25 dB deeper (3 dB × 0.75).
    assert!(o.rms_gr_db > expected + 1.5, "Dual must not read the +3 dB lift: {} vs the lifted {expected}", o.rms_gr_db);
    assert_eq!(o.peak_gr_db, 0.0, "the key is under the peak stage's threshold");
    assert_eq!(o.gr_db, o.rms_gr_db);

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
