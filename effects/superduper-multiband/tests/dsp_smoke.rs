//! DSP tests for SuperDuper Multiband, on the engine directly (no host).
//!
//! The contract: idle bands are bit-transparent (dynamic phase), a band
//! compresses only its own range, and a keyed band reacts to the bus key's
//! level in its range even when its own input is quiet.

use superduper_multiband::dsp::{band_key_db, band_range, band_ref_hz, static_gain_db, Engine, Settings, MAX_BANDS};
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

/// Render with the settings chosen per block; returns (out_l, reports).
fn render_with(x: &[f32], block: usize, mut at: impl FnMut(usize) -> Settings) -> (Vec<f32>, Vec<[f32; MAX_BANDS]>) {
    let mut e = Engine::new(SR, &at(0));
    let mut out = vec![0.0; x.len()];
    let mut scratch = vec![0.0; block];
    let mut reps = Vec::new();
    for (bi, (xi, oi)) in x.chunks(block).zip(out.chunks_mut(block)).enumerate() {
        let n = xi.len();
        reps.push(e.process(&at(bi * block), &NO_KEY, xi, xi, oi, &mut scratch[..n]).gr_db);
    }
    (out, reps)
}

/// Largest |4th difference| in `x[a..b]`: a high-pass (+24 dB/oct) under
/// which a step is ~6× its size while a tone at f is ×(2·sin(πf/sr))⁴ — so
/// a click stands out, a smooth gain change does not. The tones used here sit
/// low; the output of a downward compressor is never louder than the input,
/// so the input's own figure is the ceiling.
fn max_click(x: &[f32], a: usize, b: usize) -> f32 {
    (a.max(4)..b).map(|i| (x[i] - 4.0 * x[i - 1] + 6.0 * x[i - 2] - 4.0 * x[i - 3] + x[i - 4]).abs()).fold(0.0, f32::max)
}

#[test]
fn narrow_bands_get_the_gain_they_ask_for() {
    // Band 2 asks −12 dB (as makeup, so the request is exact). The shelves'
    // tails overlap when crossovers are close; the solver must still land the
    // band centre on −12 and leave the neighbours alone.
    for oct in [1.0f32, 1.0 / 3.0] {
        let c0 = 500.0;
        let mut s = Settings { bands: 3, xover_hz: [c0, c0 * 2f32.powf(oct), 16000.0], ..Default::default() };
        for b in s.band.iter_mut() { b.threshold_db = 0.0; b.knee_db = 0.0; }
        s.band[1].makeup_db = -12.0;
        let fs: Vec<f32> = (0..3).map(|b| band_ref_hz(3, &s.xover_hz, b, SR)).collect();
        let n = SR as usize;
        let x: Vec<f32> = (0..n).map(|i| fs.iter().map(|f| 0.05 * (std::f32::consts::TAU * f * i as f32 / SR).sin()).sum()).collect();
        let (out, _) = render(&s, &NO_KEY, &x);
        let d: Vec<f32> = fs.iter().map(|&f| level_db(&out, f, n / 2) - level_db(&x, f, n / 2)).collect();
        println!("{oct:.2} oct: {:.0}/{:.0}/{:.0} Hz → {:+.2} {:+.2} {:+.2} dB", fs[0], fs[1], fs[2], d[0], d[1], d[2]);
        assert!((d[1] + 12.0).abs() < 1.0, "{oct:.2} oct: band 2 got {:+.2} dB, asked −12", d[1]);
        assert!(d[0].abs() < 1.0 && d[2].abs() < 1.0, "{oct:.2} oct: neighbours moved ({:+.2}, {:+.2})", d[0], d[2]);
    }
}

#[test]
fn narrow_band_meter_reports_what_the_audio_got() {
    // A ⅓-octave band compressing: the meter's GR is the cut the tone really
    // gets (within 1 dB), whatever the solver could or could not reach.
    let c0 = 1000.0;
    let mut s = Settings { bands: 3, xover_hz: [c0, c0 * 1.26, 16000.0], ..Default::default() };
    for b in s.band.iter_mut() { b.threshold_db = 0.0; }
    s.band[1].threshold_db = -40.0;
    s.band[1].ratio = 20.0;
    s.band[1].knee_db = 0.0;
    s.band[1].range_db = 0.0;
    let f = band_ref_hz(3, &s.xover_hz, 1, SR);
    let n = SR as usize;
    let x = tone(f, 0.3, n);
    let (out, gr) = render(&s, &NO_KEY, &x);
    let d = level_db(&out, f, n / 4) - level_db(&x, f, n / 4);
    println!("⅓-oct band: meter {:+.2} dB, tone {d:+.2} dB", gr[1]);
    assert!(d < -10.0, "the band should be cut (got {d:+.2})");
    assert!((d - gr[1]).abs() < 1.0, "meter {:+.2} vs measured {d:+.2}", gr[1]);
}

#[test]
fn unsolo_does_not_click() {
    // Band 1 compressing, solo it for a while, unsolo: the staircase kept
    // running and the switch crossfades, so no step at either transition.
    let n = SR as usize * 2;
    let x: Vec<f32> = (0..n).map(|i| {
        let t = i as f32 / SR;
        0.5 * (std::f32::consts::TAU * 60.0 * t).sin() + 0.2 * (std::f32::consts::TAU * 1500.0 * t).sin()
    }).collect();
    let mut base = Settings { bands: 3, xover_hz: [200.0, 4000.0, 8000.0], ..Default::default() };
    for b in base.band.iter_mut() { b.threshold_db = 0.0; }
    base.band[0].threshold_db = -30.0;
    base.band[0].ratio = 6.0;
    let (solo_at, unsolo_at) = (SR as usize / 2, SR as usize);
    let (out, _) = render_with(&x, 128, |i| {
        let mut s = base;
        s.band[0].solo = (solo_at..unsolo_at).contains(&i);
        s
    });
    let ceiling = max_click(&x, 0, n);
    let at_solo = max_click(&out, solo_at - 10, solo_at + 960);
    let at_unsolo = max_click(&out, unsolo_at - 10, unsolo_at + 960);
    println!("4th diff: input {ceiling:.5}, solo {at_solo:.5}, unsolo {at_unsolo:.5}");
    let steady = ceiling;
    assert!(at_solo < steady * 1.2, "click entering solo ({at_solo:.4} vs {steady:.4})");
    assert!(at_unsolo < steady * 1.2, "click leaving solo ({at_unsolo:.4} vs {steady:.4})");
}

#[test]
fn band_count_change_while_playing_does_not_click() {
    // 4 → 2 → 4 bands with every band compressing. Re-enabled bands start
    // from GR 0 (not a stale envelope) and the audio glides between layouts.
    let n = SR as usize * 3;
    let fs = [50.0f32, 200.0, 800.0, 3200.0];
    let x: Vec<f32> = (0..n).map(|i| fs.iter().map(|f| 0.2 * (std::f32::consts::TAU * f * i as f32 / SR).sin()).sum()).collect();
    let mut base = Settings { bands: 4, xover_hz: [100.0, 400.0, 1600.0], ..Default::default() };
    for b in base.band.iter_mut() { b.threshold_db = -30.0; b.ratio = 4.0; b.knee_db = 0.0; }
    let block = 32;
    let (to2, to4) = (SR as usize, SR as usize * 2);
    let (out, reps) = render_with(&x, block, |i| {
        let mut s = base;
        s.bands = if (to2..to4).contains(&i) { 2 } else { 4 };
        s
    });
    let steady = max_click(&x, 0, n);
    let at2 = max_click(&out, to2 - 10, to2 + 960);
    let at4 = max_click(&out, to4 - 10, to4 + 960);
    println!("4th diff: input {steady:.5}, 4→2 {at2:.5}, 2→4 {at4:.5}");
    assert!(at2 < steady * 1.2, "click at 4→2 ({at2:.4} vs {steady:.4})");
    assert!(at4 < steady * 1.2, "click at 2→4 ({at4:.4} vs {steady:.4})");
    let before = reps[to2 / block - 1];
    let back = reps[to4 / block];
    println!("GR before {before:?}, first block back {back:?}");
    assert!(before[3] < -5.0, "band 4 should be compressing before the switch");
    assert!(back[3].abs() < 1.0, "band 4 must restart from GR 0, got {:.2}", back[3]);
}
