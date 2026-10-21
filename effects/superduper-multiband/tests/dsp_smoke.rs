//! DSP tests for SuperDuper Multiband, on the engine directly (no host).
//!
//! The contract: idle bands are bit-transparent (dynamic phase), a band
//! compresses only its own range, and a keyed band reacts to the bus key's
//! level in its range even when its own input is quiet.

use superduper_multiband::dsp::{band_key_db, band_range, static_gain_db, Engine, Settings, MAX_BANDS};
use superduper_synth_core::gui::LiveScope;

const SR: f32 = 48_000.0;
const BLOCK: usize = 512;

fn tone(hz: f32, amp: f32, n: usize) -> Vec<f32> {
    (0..n).map(|i| amp * (std::f32::consts::TAU * hz * i as f32 / SR).sin()).collect()
}

fn noise(n: usize, amp: f32) -> Vec<f32> {
    let mut s = 0x1234_5678u32;
    (0..n).map(|_| { s ^= s << 13; s ^= s >> 17; s ^= s << 5; amp * ((s as f32 / u32::MAX as f32) * 2.0 - 1.0) }).collect()
}

/// Render in host-sized blocks; returns (out_l, last block's report GR per band).
fn render(s: &Settings, key: &[f32; MAX_BANDS], x: &[f32]) -> (Vec<f32>, [f32; MAX_BANDS]) {
    let mut e = Engine::new(SR, s);
    let mut out = vec![0.0; x.len()];
    let mut scratch = vec![0.0; BLOCK];
    let mut gr = [0.0; MAX_BANDS];
    for (xi, oi) in x.chunks(BLOCK).zip(out.chunks_mut(BLOCK)) {
        let n = xi.len();
        gr = e.process(s, key, xi, xi, oi, &mut scratch[..n]).gr_db;
    }
    (out, gr)
}

/// Level of one frequency component (dB), Goertzel over the last `n` samples.
fn level_db(x: &[f32], hz: f32, n: usize) -> f32 {
    let x = &x[x.len() - n..];
    let w = std::f32::consts::TAU * hz / SR;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, v) in x.iter().enumerate() {
        re += (*v as f64) * ((w * i as f32).cos() as f64);
        im -= (*v as f64) * ((w * i as f32).sin() as f64);
    }
    (20.0 * ((re * re + im * im).sqrt() * 2.0 / n as f64).log10()) as f32
}

const NO_KEY: [f32; MAX_BANDS] = [-120.0; MAX_BANDS];

#[test]
fn idle_bands_reconstruct_the_input_exactly() {
    // Loud noise, every band count, nothing over threshold: output == input.
    let x = noise(SR as usize, 0.5);
    for bands in 2..=4 {
        let mut s = Settings { bands, ..Default::default() };
        for b in s.band.iter_mut() { b.threshold_db = 0.0; b.knee_db = 0.0; }
        let (out, _) = render(&s, &NO_KEY, &x);
        let err = x.iter().zip(&out).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
        let err_db = 20.0 * err.max(1e-12).log10();
        println!("{bands} bands idle: max |out − in| = {err_db:.1} dB");
        assert!(err_db < -100.0, "{bands} bands: idle path not transparent ({err_db:.1} dB)");
    }
}

#[test]
fn bypassed_bands_with_loud_input_are_transparent() {
    let x = noise(SR as usize / 2, 0.9);
    let mut s = Settings { bands: 4, ..Default::default() };
    for b in s.band.iter_mut() { b.bypass = true; b.threshold_db = -60.0; b.makeup_db = 6.0; }
    let (out, _) = render(&s, &NO_KEY, &x);
    assert!(x.iter().zip(&out).all(|(a, b)| a == b), "bypassed bands must be bit-identical");
}

#[test]
fn transparent_again_after_compression_releases() {
    // A loud burst compresses band 1, then a quiet tail: once the detector
    // falls below threshold the output must be the input again, exactly.
    let mut x = tone(80.0, 0.9, SR as usize / 2);
    x.extend(noise(SR as usize, 0.01));
    let mut s = Settings::default();
    s.band[0].threshold_db = -20.0;
    s.band[0].release_ms = 20.0;
    let (out, _) = render(&s, &NO_KEY, &x);
    let tail = x.len() - SR as usize / 4;
    assert!(out[..SR as usize / 2].iter().zip(&x).any(|(a, b)| a != b), "the burst should have been compressed");
    let err = x[tail..].iter().zip(&out[tail..]).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(err == 0.0, "tail not bit-identical after release (max err {err:e})");
}

#[test]
fn a_band_compresses_only_its_own_range() {
    // Band 1 (< 200 Hz) hard, bands 2 and 3 with thresholds out of reach.
    let mut s = Settings { bands: 3, xover_hz: [200.0, 2000.0, 8000.0], ..Default::default() };
    s.band[0].threshold_db = -30.0;
    s.band[0].ratio = 8.0;
    s.band[0].knee_db = 0.0;
    s.band[1].threshold_db = 0.0;
    s.band[2].threshold_db = 0.0;
    let n = SR as usize;
    let lo = tone(60.0, 0.4, n);
    let hi = tone(6000.0, 0.4, n);
    let x: Vec<f32> = lo.iter().zip(&hi).map(|(a, b)| a + b).collect();
    let (out, gr) = render(&s, &NO_KEY, &x);
    let win = SR as usize / 2;
    let d_lo = level_db(&out, 60.0, win) - level_db(&x, 60.0, win);
    let d_hi = level_db(&out, 6000.0, win) - level_db(&x, 6000.0, win);
    println!("band 1 GR {:.1} dB → 60 Hz {d_lo:+.2} dB, 6 kHz {d_hi:+.3} dB", gr[0]);
    assert!(d_lo < -10.0, "the tone in band 1 should be reduced (got {d_lo:+.2} dB)");
    assert!(d_hi.abs() < 0.1, "the tone in band 3 should be untouched (got {d_hi:+.3} dB)");
    assert_eq!(gr[2], 0.0, "band 3 must not react");
    // The measured cut matches the band's gain reduction (the shelf staircase
    // gives band 1 its gain, not an approximation of it).
    assert!((d_lo - gr[0]).abs() < 1.0, "cut {d_lo:.2} vs GR {:.2}", gr[0]);
}

#[test]
fn a_middle_band_cuts_only_between_its_crossovers() {
    // The shelf pair (+g at the lower crossover, −g at the upper) must
    // cancel outside the band: lows and highs stay where they were.
    let mut s = Settings { bands: 4, xover_hz: [200.0, 2000.0, 8000.0], ..Default::default() };
    for b in s.band.iter_mut() { b.threshold_db = 0.0; }
    s.band[1].threshold_db = -40.0;
    s.band[1].ratio = 10.0;
    s.band[1].knee_db = 0.0;
    let n = SR as usize;
    let fs = [50.0, 630.0, 12000.0];
    let x: Vec<f32> = (0..n).map(|i| fs.iter().map(|f| 0.3 * (std::f32::consts::TAU * f * i as f32 / SR).sin()).sum()).collect();
    let (out, gr) = render(&s, &NO_KEY, &x);
    let d: Vec<f32> = fs.iter().map(|&f| level_db(&out, f, n / 2) - level_db(&x, f, n / 2)).collect();
    println!("band 2 GR {:.1} dB → 50 Hz {:+.2}, 630 Hz {:+.2}, 12 kHz {:+.2}", gr[1], d[0], d[1], d[2]);
    assert!(d[1] < -10.0 && (d[1] - gr[1]).abs() < 1.5, "630 Hz follows band 2's GR");
    assert!(d[0].abs() < 0.3 && d[2].abs() < 0.3, "outside the band nothing moves");
}

#[test]
fn keyed_band_ducks_from_the_bus_key_even_with_quiet_input() {
    // The key track is loud between ~300 Hz and 3 kHz, silent elsewhere.
    let scope = LiveScope::new(64);
    let edges = superduper_dsp_sdk::bus::band_edges();
    for (b, a) in scope.key_bands.iter().enumerate() {
        let centre = (edges[b] * edges[b + 1]).sqrt();
        a.store(if (300.0..3000.0).contains(&centre) { -10.0 } else { -120.0 }, std::sync::atomic::Ordering::Relaxed);
    }
    let mut s = Settings { bands: 3, xover_hz: [250.0, 4000.0, 8000.0], ..Default::default() };
    for b in s.band.iter_mut() { b.threshold_db = -30.0; b.ratio = 4.0; b.knee_db = 0.0; }
    s.band[1].key = 1.0; // the mid band ducks from the key; low/high compress on their own signal
    let key: [f32; MAX_BANDS] = std::array::from_fn(|b| {
        let (lo, hi) = band_range(s.bands, &s.xover_hz, b);
        band_key_db(&scope, lo, hi)
    });
    println!("key per band: {key:?}");
    assert!(key[1] > -12.0 && key[0] < -100.0, "the key is read in the mid band's range only");

    // Quiet input (−60 dBFS at 1 kHz): far below threshold on its own.
    let x = tone(1000.0, 0.001, SR as usize / 2);
    let (_, gr_keyed) = render(&s, &key, &x);
    let mut unkeyed = s;
    unkeyed.band[1].key = 0.0;
    let (_, gr_plain) = render(&unkeyed, &key, &x);
    println!("mid GR keyed {:.1} dB, unkeyed {:.1} dB", gr_keyed[1], gr_plain[1]);
    assert!(gr_keyed[1] < -10.0, "keyed band should duck (GR {:.1})", gr_keyed[1]);
    assert_eq!(gr_plain[1], 0.0, "without the key the quiet band does nothing");
    assert_eq!(gr_keyed[0], 0.0, "unkeyed low band is unaffected by the key");
    assert_eq!(gr_keyed[2], 0.0, "unkeyed high band is unaffected by the key");
}

#[test]
fn static_curve_shape() {
    assert_eq!(static_gain_db(-40.0, -20.0, 4.0, 6.0, 0.0), 0.0, "below the knee: exactly 0");
    assert!((static_gain_db(0.0, -20.0, 4.0, 0.0, 0.0) + 15.0).abs() < 1e-4, "20 dB over at 4:1 → −15");
    assert_eq!(static_gain_db(0.0, -20.0, 4.0, 0.0, 6.0), -6.0, "Range caps the reduction");
    assert_eq!(static_gain_db(0.0, -20.0, 1.0, 6.0, 0.0), 0.0, "1:1 does nothing");
    let up = static_gain_db(-10.0, -20.0, 0.5, 0.0, 0.0);
    assert!((up - 10.0).abs() < 1e-4, "0.5:1 is upward: +10 dB at 10 over (got {up})");
    assert_eq!(static_gain_db(20.0, -20.0, 0.5, 0.0, 0.0), 12.0, "upward is capped without Range");
    let mut prev = 0.0;
    for x in -40..0 {
        let g = static_gain_db(x as f32, -20.0, 4.0, 12.0, 0.0);
        assert!(g <= prev + 1e-5, "monotonic through the knee");
        prev = g;
    }
}
