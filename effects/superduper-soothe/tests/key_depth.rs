//! Key Depth measures Key Depth: a broadband key at full factor cuts every
//! band centre by Key Depth dB, whatever the Q.
//!
//! The 24 peaking cuts cascade 0.245 octave apart and their skirts overlap,
//! so before 2026-10-21 a 12 dB key cut on every band measured −25.7 dB at
//! the centres at Q 5 (the default), −55 dB at Q 2 and a −15/−8 dB comb at
//! Q 12. The engine now scales the key cut per band by the cascade's
//! response (`Engine::key_scale`), computed when the banks are rebuilt.
//!
//! Driven on the engine directly (no host), with the bus key stubbed as
//! loud everywhere and Amount 0 so the resonance path stays out of it.

use superduper_soothe::{BlockParams, Engine, N_BANDS};

const SR: f32 = 48_000.0;
const BLOCK: usize = 512;

fn noise(n: usize, amp: f32) -> Vec<f32> {
    let mut s = 0x2545_F491u32;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            amp * ((s as f32 / u32::MAX as f32) * 2.0 - 1.0)
        })
        .collect()
}

/// |X(f)|² of `x` under a Hann window (the window keeps the filters' edge
/// transients out of the estimate, so Y/X reads |H| whatever the noise).
fn hann_power(x: &[f32], hz: f32) -> f64 {
    let n = x.len();
    let w = core::f64::consts::TAU * hz as f64 / SR as f64;
    let (mut re, mut im) = (0.0f64, 0.0f64);
    for (i, v) in x.iter().enumerate() {
        let hann = 0.5 - 0.5 * (core::f64::consts::TAU * i as f64 / n as f64).cos();
        let (s, c) = (w * i as f64).sin_cos();
        re += *v as f64 * hann * c;
        im -= *v as f64 * hann * s;
    }
    re * re + im * im
}

/// Gain (dB) of `out` over `inp` at `hz`: 1 s Hann windows every ½ s over
/// the last 3 s, powers summed before the ratio so a spectral null of this
/// noise realisation in one window cannot throw it.
fn gain_at(inp: &[f32], out: &[f32], hz: f32) -> f32 {
    let (win, hop) = (SR as usize, SR as usize / 2);
    let start = inp.len() - 3 * SR as usize;
    let (mut px, mut py) = (0.0, 0.0);
    let mut a = start;
    while a + win <= inp.len() {
        px += hann_power(&inp[a..a + win], hz);
        py += hann_power(&out[a..a + win], hz);
        a += hop;
    }
    (10.0 * (py / px).log10()) as f32
}

/// Render 4 s of noise through a fresh engine at `q` with a broadband key
/// (0 dBFS everywhere) at Key Depth `depth`, Amount 0.
fn render_keyed(q: f32, depth: f32) -> (Vec<f32>, Vec<f32>, [f32; N_BANDS]) {
    let mut e = Engine::new(SR, 300.0, 10_000.0, q, 0.0, -6.0, 1.0, 0.0);
    let p = BlockParams { amount: 0.0, sens: -6.0, mix: 1.0, output_db: 0.0, ratio: 0.7, attack_ms: 5.0, release_ms: 80.0 };
    let key_cut = e.key_cuts(|_| 0.0, depth, 24.0);
    let x = noise(4 * SR as usize, 0.3);
    let mut out = vec![0.0; x.len()];
    let mut scratch = vec![0.0; BLOCK];
    for (xi, oi) in x.chunks(BLOCK).zip(out.chunks_mut(BLOCK)) {
        e.process_block(xi, xi, oi, Some(&mut scratch[..xi.len()]), &p, &key_cut, None);
    }
    (x, out, *e.band_centres())
}

#[test]
fn broadband_key_at_full_depth_cuts_every_band_centre_by_key_depth() {
    let depth = 12.0;
    for q in [5.0f32, 2.0, 12.0] {
        let (x, out, centres) = render_keyed(q, depth);
        let g: Vec<f32> = centres.iter().map(|&f| gain_at(&x, &out, f)).collect();
        let (lo, hi) = g.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(l, h), &v| (l.min(v), h.max(v)));
        let e = Engine::new(SR, 300.0, 10_000.0, q, 0.0, -6.0, 1.0, 0.0);
        let s: Vec<String> = e.key_scale().iter().map(|v| format!("{v:.2}")).collect();
        println!("Q {q}: key depth {depth} dB on all 24 bands measures {lo:+.2} .. {hi:+.2} dB at the band centres; scale {}", s.join(" "));
        for (b, (&f, &v)) in centres.iter().zip(&g).enumerate() {
            assert!((v + depth).abs() <= 1.0, "Q {q}, band {b} at {f:.0} Hz: {v:+.2} dB, asked −{depth}");
        }
    }
}

#[test]
fn no_key_leaves_the_cascade_at_unity() {
    // Key Depth 0 → no cut at all: 24 peaking EQs at 0 dB, unity to within
    // their coefficient rounding (RBJ normalises by 1/a0, one ulp a stage).
    let (x, out, _) = render_keyed(5.0, 0.0);
    let err = x.iter().zip(&out).fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(err < 1e-5, "with no key the engine is transparent at Amount 0 (max |out − in| = {err:e})");
}
