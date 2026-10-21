//! DSP tests for SuperDuper Multiband, on the engine directly (no host).
//!
//! The contract: idle bands are bit-transparent (dynamic phase), a band
//! compresses only its own range, and a keyed band reacts to the bus key's
//! level in its range even when its own input is quiet.

use superduper_multiband::dsp::{band_key_db, band_range, band_ref_hz, static_gain_db, Engine, Settings, SolverProbe, MAX_BANDS, MIN_XOVER_RATIO};
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
    for oct in [1.0f32, 0.5] {
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
    // A ½-octave band (the narrowest the sanitiser allows) compressing: the
    // meter's GR is the cut the tone really gets (within 1 dB), whatever the
    // solver could or could not reach.
    let c0 = 1000.0;
    let mut s = Settings { bands: 3, xover_hz: [c0, c0 * MIN_XOVER_RATIO, 16000.0], ..Default::default() };
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
    println!("½-oct band: meter {:+.2} dB, tone {d:+.2} dB", gr[1]);
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

// ---------------------------------------------------------------------------
// Review findings of 2026-10-21, each pinned by the test that was red first.
// ---------------------------------------------------------------------------

#[test]
fn band_count_change_resets_the_old_top_bands_envelope() {
    // Same layout as the click test. In the 2-band stretch band 2 spans
    // 100 Hz..20 kHz and holds three tones; back at 4 bands it spans
    // 100..400 Hz and holds one. Its first block back must start from a fresh
    // detector, not from the wide-band envelope it kept (−15.7 dB on that
    // block before the fix, against −12 dB at steady state): a band whose
    // range changed is reset like a band that is new.
    let n = SR as usize * 3;
    let fs = [50.0f32, 200.0, 800.0, 3200.0];
    let x: Vec<f32> = (0..n).map(|i| fs.iter().map(|f| 0.2 * (std::f32::consts::TAU * f * i as f32 / SR).sin()).sum()).collect();
    let mut base = Settings { bands: 4, xover_hz: [100.0, 400.0, 1600.0], ..Default::default() };
    for b in base.band.iter_mut() { b.threshold_db = -30.0; b.ratio = 4.0; b.knee_db = 0.0; }
    let block = 32;
    let (to2, to4) = (SR as usize, SR as usize * 2);
    let (_, reps) = render_with(&x, block, |i| {
        let mut s = base;
        s.bands = if (to2..to4).contains(&i) { 2 } else { 4 };
        s
    });
    let wide = reps[to4 / block - 1];
    let back = reps[to4 / block];
    let steady = reps[(to4 + SR as usize / 2) / block];
    println!("2→4: band 2 wide-band {:.2} dB, first block back {:.2} dB, 0.5 s later {:.2} dB", wide[1], back[1], steady[1]);
    assert!(steady[1] < -5.0, "band 2 should be compressing its own tone again ({:.2})", steady[1]);
    assert!(back[1] > -1.0, "band 2 resumed its wide-band envelope: {:.2} dB on the first block back", back[1]);
}

#[test]
fn a_nan_or_infinite_key_does_not_latch_the_detectors() {
    // `key_db_at` can hand back NaN or ±inf when a neighbouring bus band is
    // ±inf. One such block must neither poison the audio nor latch the
    // key-blended detectors: once the key is sane again, the keyed bands duck.
    let mut s = Settings { bands: 3, xover_hz: [250.0, 4000.0, 8000.0], ..Default::default() };
    for b in s.band.iter_mut() { b.threshold_db = -30.0; b.ratio = 4.0; b.knee_db = 0.0; }
    s.band[0].key = 0.5;
    s.band[1].key = 1.0;
    let x = tone(1000.0, 0.001, SR as usize);
    let mut e = Engine::new(SR, &s);
    let (mut out, mut scratch) = (vec![0.0; BLOCK], vec![0.0; BLOCK]);
    let bad = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, f32::NAN];
    let rep = e.process(&s, &bad, &x[..BLOCK], &x[..BLOCK], &mut out, &mut scratch);
    assert!(out.iter().all(|v| v.is_finite()), "a bad key reached the audio");
    assert!(rep.gr_db.iter().all(|g| g.is_finite()), "a bad key reached the meter: {:?}", rep.gr_db);
    let loud = [-10.0; MAX_BANDS];
    let mut gr = [0.0; MAX_BANDS];
    for c in x.chunks(BLOCK).skip(1) {
        gr = e.process(&s, &loud, c, c, &mut out[..c.len()], &mut scratch[..c.len()]).gr_db;
    }
    println!("one NaN/inf key block, then a −10 dB key for 1 s: GR {gr:?}");
    assert!(out.iter().all(|v| v.is_finite()));
    assert!(gr[1] < -10.0, "the keyed band stayed latched off (GR {:.2})", gr[1]);
    assert!(gr[0] < -5.0, "the half-keyed band stayed latched off (GR {:.2})", gr[0]);
}

/// 2000 random band-gain sets in `lo..lo+span` dB on `bands` bands whose
/// crossovers sit `oct` octaves apart, each held for 8 samples (a jump, then
/// the gains hold, as a transient does). Returns (held targets more than
/// 1 dB off with a shelf within 1 dB of its ±72 dB limit — the layout cannot
/// deliver them, no solver can; the same with every shelf free — the solver
/// did not find it; worst free error).
fn solver_misses(bands: usize, oct: f32, lo: f32, span: f32) -> (usize, usize, f32) {
    let xover = [1000.0f32, 1000.0 * 2f32.powf(oct), 1000.0 * 2f32.powf(2.0 * oct)];
    let mut p = SolverProbe::new(SR, &xover, bands);
    let mut seed = 0x9E37_79B9u32;
    let mut rnd = move || { seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5; seed as f32 / u32::MAX as f32 };
    let (mut pinned, mut free, mut worst_free) = (0, 0, 0.0f32);
    for _ in 0..2000 {
        let t: [f32; MAX_BANDS] = std::array::from_fn(|b| if b < bands { lo + span * rnd() } else { 0.0 });
        let mut e = 0.0;
        for _ in 0..8 {
            let (target, applied) = p.solve(&t);
            e = (0..bands).map(|b| (applied[b] - target[b]).abs()).fold(0.0f32, f32::max);
        }
        let (sh, lim) = p.shelves();
        if e > 1.0 {
            if sh[..bands - 1].iter().any(|v| v.abs() >= lim - 1.0) { pinned += 1; } else { free += 1; worst_free = worst_free.max(e); }
        }
    }
    (pinned, free, worst_free)
}

#[test]
fn solver_lands_every_target_the_shelves_can_reach() {
    // ⅓ octave × 4 bands, ordinary 0..−12 dB gains, through the raw solver
    // (the sanitiser no longer allows the layout — this is the stress case).
    // Before 2026-10-21: four undamped iterations and a held target reused
    // as it was → 251 of 2000 held targets more than 1 dB off with every
    // shelf free, the worst by 11.8 dB. Damped Newton that carries on while
    // the gains hold, with a flat restart when the warm start is in the
    // wrong basin, leaves 20 at ≤ 1.8 dB — the layout's own limit (a
    // 32-start search finds nothing better for them).
    let (pinned, free, worst) = solver_misses(4, 1.0 / 3.0, -12.0, 12.0);
    println!("⅓ oct × 4 bands, 0..−12 dB, 2000 held targets: {pinned} unreachable (shelf at ±72 dB), {free} missed (worst {worst:.1} dB)");
    assert!(pinned + free <= 40 && worst <= 2.0, "{} of 2000 more than 1 dB off (worst {worst:.1} dB)", pinned + free);
}

#[test]
fn half_octave_layouts_deliver_every_gain_reduction_combination() {
    // At the sanitiser's minimum spacing every combination lands, at 3 and
    // 4 bands, for plain GR (0..−12 dB), GR with makeup (−12..+6) and the
    // full −24..+6 at 3 bands. The same layouts at ⅓ octave read 282 and
    // 891 misses (4 bands, worst 6.2 and 9.7 dB) with this solver, which is
    // why ½ octave is the floor.
    let oct = MIN_XOVER_RATIO.log2();
    for (bands, lo, span) in [(3usize, -12.0f32, 12.0f32), (4, -12.0, 12.0), (4, -12.0, 18.0), (3, -24.0, 30.0)] {
        let (pinned, free, worst) = solver_misses(bands, oct, lo, span);
        println!("½ oct × {bands} bands, {lo}..{} dB: {pinned} unreachable, {free} missed (worst {worst:.1} dB)", lo + span);
        assert_eq!((pinned, free), (0, 0), "{bands} bands {lo}..{} dB: {pinned} unreachable, {free} missed (worst {worst:.1})", lo + span);
    }
}

#[test]
fn key_levels_follow_the_sanitised_crossovers() {
    // Crossovers out of order from the host (a dragged handle, automation)
    // are sanitised by the engine to [4000, 5040, 8000]. The key must be read
    // over those bands: raw band 3 would span 250 Hz..20 kHz and hear a key
    // that lives in 300 Hz..3 kHz, a range the sanitised band 3 never covers.
    let scope = LiveScope::new(64);
    let edges = superduper_dsp_sdk::bus::band_edges();
    for (b, a) in scope.key_bands.iter().enumerate() {
        let centre = (edges[b] * edges[b + 1]).sqrt();
        a.store(if (300.0..3000.0).contains(&centre) { -10.0 } else { -120.0 }, std::sync::atomic::Ordering::Relaxed);
    }
    let s = Settings { bands: 3, xover_hz: [4000.0, 250.0, 8000.0], ..Default::default() };
    let k = superduper_multiband::dsp::key_levels(&scope, &s, SR);
    let san = s.sanitized(SR);
    let want: [f32; MAX_BANDS] = std::array::from_fn(|b| {
        let (lo, hi) = band_range(san.bands, &san.xover_hz, b);
        band_key_db(&scope, lo, hi)
    });
    println!("raw {:?} → sanitised {:?}: key {k:?}", s.xover_hz, san.xover_hz);
    assert_eq!(k, want, "key read over the raw crossovers");
    assert!(k[0] > -12.0 && k[1] < -100.0 && k[2] < -100.0, "{k:?}");
}
