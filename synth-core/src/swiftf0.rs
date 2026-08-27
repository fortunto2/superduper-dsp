//! SwiftF0 neural pitch tracker — pure-Rust streaming inference.
//!
//! SwiftF0 (lars76/swift-f0, CC-BY-4.0) is a 97k-parameter CNN over a
//! log-magnitude STFT patch that outscores classical trackers on accuracy
//! (its benchmark: 90% vs pYIN's 79%). The published ONNX carries an STFT
//! node no plugin-friendly runtime accepts, so this module implements the
//! whole pipeline directly — resample → STFT → CNN → decode — the same
//! split verified bit-exact against the reference model in
//! `~/Music/1music/swiftf0-coreml/README.md`.
//!
//! Model contract (all verified against the ONNX graph):
//! - 16 kHz mono; STFT frame 1024, hop 256 (16 ms), the model's own
//!   asymmetric Hann window; zero pad 384 at the stream head.
//! - features = ln(|rFFT| bins 3..135 + 1e-8) → column of 132.
//! - 5× Conv2d 5×5 SAME (1→8→16→32→64→1) + ReLU over (freq, time), then a
//!   1×1 Conv 132→200, softmax over 200 log-spaced bins (46.9–2093.8 Hz),
//!   pitch = probability-weighted mean of bin centers within ±9 bins of the
//!   argmax, confidence = probability mass in that window.
//!
//! **Streaming:** convolutions are computed one time-column per hop with the
//! two future taps taken as zeros — the newest column then matches what the
//! offline model computes at the right EDGE of a window (SAME zero padding),
//! which the network saw throughout training. Cached feature columns keep
//! their edge-computed values rather than being re-fixed when the future
//! arrives; the golden test in `tests/dsp_blocks.rs` bounds the resulting
//! deviation against the reference model. Zero added latency beyond the
//! STFT window itself.
//!
//! RT rules: all buffers are allocated in [`SwiftF0Tracker::new`] (call it
//! at `activate()`); `push` never allocates.

use crate::dsp_blocks::Biquad;
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Weights exported from the reference ONNX (`swiftf0-coreml/cnn.onnx`),
/// flat little-endian f32 in a fixed order — see `OFFSETS`.
static WEIGHTS: &[u8] = include_bytes!("swiftf0_weights.bin");

const N_FFT: usize = 1024;
const HOP: usize = 256;
const N_FREQ: usize = 132; // rFFT bins 3..135
const N_BINS: usize = 200;
const SR_MODEL: f32 = 16_000.0;
/// Channels per conv layer, input→output.
const CHANS: [(usize, usize); 5] = [(1, 8), (8, 16), (16, 32), (32, 64), (64, 1)];

/// (name, len) in f32 units, in blob order.
const SEGMENTS: [usize; 14] = [
    8 * 25,       // conv1 w  (Cout, Cin, kFreq=5, kTime=5)
    8,            // conv1 b
    16 * 8 * 25,  // conv2 w
    16,           // conv2 b
    32 * 16 * 25, // conv3 w
    32,           // conv3 b
    64 * 32 * 25, // conv4 w
    64,           // conv4 b
    64 * 25,      // conv5 w
    1,            // conv5 b
    N_BINS * N_FREQ, // proj w
    N_BINS,       // proj b
    N_BINS,       // pitch bin centers, Hz
    N_FFT,        // analysis window
];

struct Weights {
    conv_w: [Vec<f32>; 5],
    conv_b: [Vec<f32>; 5],
    proj_w: Vec<f32>,
    proj_b: Vec<f32>,
    centers: Vec<f32>,
    window: Vec<f32>,
}

fn load_weights() -> Weights {
    let total: usize = SEGMENTS.iter().sum();
    assert_eq!(WEIGHTS.len(), total * 4, "swiftf0_weights.bin size mismatch");
    let mut all = Vec::with_capacity(total);
    for c in WEIGHTS.chunks_exact(4) {
        all.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
    }
    let mut parts: Vec<Vec<f32>> = Vec::with_capacity(SEGMENTS.len());
    let mut at = 0usize;
    for len in SEGMENTS {
        parts.push(all[at..at + len].to_vec());
        at += len;
    }
    let mut it = parts.into_iter();
    let mut next = || it.next().unwrap();
    // Blob order interleaves weight/bias per layer: w1 b1 w2 b2 … w5 b5.
    let mut conv_w: [Vec<f32>; 5] = Default::default();
    let mut conv_b: [Vec<f32>; 5] = Default::default();
    for l in 0..5 {
        conv_w[l] = next();
        conv_b[l] = next();
    }
    Weights {
        conv_w,
        conv_b,
        proj_w: next(),
        proj_b: next(),
        centers: next(),
        window: next(),
    }
}

pub struct SwiftF0Tracker {
    w: Weights,
    // ---- resampler: input sr → 16 kHz ----
    in_sr: f32,
    lp1: Biquad,
    lp2: Biquad,
    use_lp: bool,
    rs_ring: [f32; 8],
    rs_n: u64,     // input samples consumed
    rs_next: f64,  // input-time (samples) of the next 16 kHz output
    rs_step: f64,  // in_sr / 16000
    // ---- STFT ----
    stft_ring: Vec<f32>,
    stft_write: usize,
    until_frame: usize,
    fft: Arc<dyn RealToComplex<f32>>,
    fft_in: Vec<f32>,
    spec: Vec<Complex<f32>>,
    fft_scratch: Vec<Complex<f32>>,
    // ---- conv layer input histories: [3 time cols][Cin][132] ----
    hist: [Vec<f32>; 5],
    col_a: Vec<f32>, // scratch output column, max 64*132
    col_b: Vec<f32>,
    logits: Vec<f32>,
    probs: Vec<f32>,
    // ---- outputs ----
    last_hz: f32,
    last_conf: f32,
    default_hz: f32,
}

impl SwiftF0Tracker {
    pub fn new(in_sr: f32, default_hz: f32) -> Self {
        let w = load_weights();
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(N_FFT);
        let scratch = fft.get_scratch_len();
        let use_lp = in_sr > 20_000.0;
        let mut lp1 = Biquad::default();
        let mut lp2 = Biquad::default();
        if use_lp {
            // 4th-order Butterworth-ish LP at 7 kHz before decimation.
            lp1.set_lpf(in_sr, 7000.0, 0.5412);
            lp2.set_lpf(in_sr, 7000.0, 1.3066);
        }
        Self {
            w,
            in_sr: in_sr.max(1.0),
            lp1,
            lp2,
            use_lp,
            rs_ring: [0.0; 8],
            rs_n: 0,
            rs_next: 2.0, // needs one sample beyond the interpolation center
            rs_step: in_sr.max(1.0) as f64 / SR_MODEL as f64,
            stft_ring: vec![0.0; N_FFT],
            stft_write: 0,
            // The reference pads 384 zeros ahead of the stream: the first
            // frame lands once 1024-384 = 640 real samples are in.
            until_frame: N_FFT - 384,
            fft,
            fft_in: vec![0.0; N_FFT],
            spec: vec![Complex::new(0.0, 0.0); N_FFT / 2 + 1],
            fft_scratch: vec![Complex::new(0.0, 0.0); scratch],
            hist: [
                vec![0.0; 3 * 1 * N_FREQ],
                vec![0.0; 3 * 8 * N_FREQ],
                vec![0.0; 3 * 16 * N_FREQ],
                vec![0.0; 3 * 32 * N_FREQ],
                vec![0.0; 3 * 64 * N_FREQ],
            ],
            col_a: vec![0.0; 64 * N_FREQ],
            col_b: vec![0.0; 64 * N_FREQ],
            logits: vec![0.0; N_BINS],
            probs: vec![0.0; N_BINS],
            last_hz: default_hz,
            last_conf: 0.0,
            default_hz,
        }
    }

    /// Push one input sample at the tracker's input rate. Returns `true` on
    /// the samples where a fresh estimate was produced (every 16 ms of
    /// resampled audio).
    #[inline]
    pub fn push(&mut self, x: f32) -> bool {
        let mut xf = x;
        if self.use_lp {
            xf = self.lp2.process(self.lp1.process(xf));
        }
        self.rs_ring[(self.rs_n & 7) as usize] = xf;
        self.rs_n += 1;
        let mut fresh = false;
        // Emit 16 kHz samples whose interpolation needs at most sample
        // rs_n - 1 (one sample of look-back headroom for the cubic).
        while self.rs_next + 2.0 <= (self.rs_n - 1) as f64 {
            let t = self.rs_next;
            let i1 = t.floor() as u64; // interp between i1 and i1+1
            let frac = (t - i1 as f64) as f32;
            let s = |k: i64| self.rs_ring[((i1 as i64 + k) & 7) as usize];
            let (s0, s1, s2, s3) = (s(-1), s(0), s(1), s(2));
            // Catmull-Rom cubic.
            let a = frac;
            let y = s1
                + 0.5
                    * a
                    * (s2 - s0
                        + a * (2.0 * s0 - 5.0 * s1 + 4.0 * s2 - s3
                            + a * (3.0 * (s1 - s2) + s3 - s0)));
            self.rs_next += self.rs_step;
            if self.push_16k(y) {
                fresh = true;
            }
        }
        fresh
    }

    #[inline]
    fn push_16k(&mut self, x: f32) -> bool {
        self.stft_ring[self.stft_write] = x;
        self.stft_write = (self.stft_write + 1) % N_FFT;
        self.until_frame -= 1;
        if self.until_frame > 0 {
            return false;
        }
        self.until_frame = HOP;
        self.analyze();
        true
    }

    fn analyze(&mut self) {
        // Linearise ring oldest→newest and window it.
        let start = self.stft_write; // oldest sample position
        for i in 0..N_FFT {
            let idx = (start + i) % N_FFT;
            self.fft_in[i] = self.stft_ring[idx] * self.w.window[i];
        }
        let _ = self
            .fft
            .process_with_scratch(&mut self.fft_in, &mut self.spec, &mut self.fft_scratch);
        // New input column for layer 1: ln(|X| bins 3..135 + 1e-8).
        push_hist(&mut self.hist[0], 1);
        {
            let newest = &mut self.hist[0][2 * N_FREQ..];
            for (f, slot) in newest[..N_FREQ].iter_mut().enumerate() {
                let c = self.spec[3 + f];
                *slot = (c.re * c.re + c.im * c.im).sqrt().max(0.0).ln_eps();
            }
        }
        // Conv stack, one column per layer.
        for l in 0..5 {
            let (cin, cout) = CHANS[l];
            let out = if l % 2 == 0 { &mut self.col_a } else { &mut self.col_b };
            conv_col(
                out,
                &self.w.conv_w[l],
                &self.w.conv_b[l],
                &self.hist[l],
                cin,
                cout,
            );
            for v in out[..cout * N_FREQ].iter_mut() {
                *v = v.max(0.0); // ReLU
            }
            if l < 4 {
                push_hist(&mut self.hist[l + 1], cout);
                let dst = &mut self.hist[l + 1][2 * cout * N_FREQ..];
                dst[..cout * N_FREQ].copy_from_slice(&out[..cout * N_FREQ]);
            }
        }
        // Final column [1][132] → 1x1 projection to 200 logits.
        let feat = &self.col_a; // layer 5 (index 4) wrote col_a
        for k in 0..N_BINS {
            let wrow = &self.w.proj_w[k * N_FREQ..(k + 1) * N_FREQ];
            let mut acc = self.w.proj_b[k];
            for f in 0..N_FREQ {
                acc += wrow[f] * feat[f];
            }
            self.logits[k] = acc;
        }
        // Softmax → ±9-bin weighted decode (matches the reference graph).
        let mx = self.logits.iter().cloned().fold(f32::MIN, f32::max);
        let mut sum = 0.0f32;
        for k in 0..N_BINS {
            let e = (self.logits[k] - mx).exp();
            self.probs[k] = e;
            sum += e;
        }
        let inv = 1.0 / sum.max(1e-12);
        let mut arg = 0usize;
        let mut best = -1.0f32;
        for k in 0..N_BINS {
            self.probs[k] *= inv;
            if self.probs[k] > best {
                best = self.probs[k];
                arg = k;
            }
        }
        let lo = arg.saturating_sub(9);
        let hi = (arg + 9).min(N_BINS - 1);
        let mut conf = 0.0f32;
        let mut wsum = 0.0f32;
        for k in lo..=hi {
            conf += self.probs[k];
            wsum += self.probs[k] * self.w.centers[k];
        }
        self.last_conf = conf;
        self.last_hz = wsum / (conf + 1e-7);
    }

    /// Latest pitch estimate in Hz (starts at `default_hz`). Gate on
    /// [`confidence`] (< ~0.5 means unvoiced) before trusting it.
    #[inline]
    pub fn current_hz(&self) -> f32 {
        self.last_hz
    }

    /// Probability mass around the winning pitch bin, 0..1.
    #[inline]
    pub fn confidence(&self) -> f32 {
        self.last_conf
    }

    pub fn reset(&mut self) {
        for h in self.hist.iter_mut() {
            h.fill(0.0);
        }
        self.stft_ring.fill(0.0);
        self.stft_write = 0;
        self.until_frame = N_FFT - 384;
        self.rs_ring = [0.0; 8];
        self.rs_n = 0;
        self.rs_next = 2.0;
        self.last_hz = self.default_hz;
        self.last_conf = 0.0;
    }
}

/// Shift a [3][C][132] history one column left, leaving the newest slot to
/// be overwritten by the caller.
fn push_hist(h: &mut [f32], c: usize) {
    let stride = c * N_FREQ;
    h.copy_within(stride..3 * stride, 0);
}

/// One SAME-padded 5×5 conv output column at the newest time position, with
/// the two future time taps as zeros. `hist` is [3][cin][132] (t-2, t-1, t);
/// `w` is [cout][cin][kFreq=5][kTime=5].
///
/// Tiled for throughput: 132 frequencies = 11 tiles of 12, the accumulator
/// tile lives in registers across the whole (kt, ci, kf) reduction, and the
/// haloed input windows (12+4) are built once per tile and reused by every
/// output channel. The naive axpy formulation was memory-bound at ~1.4×
/// realtime for the full stack; this is compute-bound.
const TILE: usize = 12;
const HALO: usize = TILE + 4;

fn conv_col(out: &mut [f32], w: &[f32], b: &[f32], hist: &[f32], cin: usize, cout: usize) {
    debug_assert_eq!(N_FREQ % TILE, 0);
    let mut win = [[0.0f32; HALO]; 3 * 64]; // 3 time cols × max cin
    for tile in 0..N_FREQ / TILE {
        let base = (tile * TILE) as isize;
        for kt in 0..3usize {
            for ci in 0..cin {
                let icol = &hist[(kt * cin + ci) * N_FREQ..(kt * cin + ci + 1) * N_FREQ];
                let wdst = &mut win[kt * cin + ci];
                for (j, slot) in wdst.iter_mut().enumerate() {
                    let fi = base + j as isize - 2;
                    *slot = if (0..N_FREQ as isize).contains(&fi) {
                        icol[fi as usize]
                    } else {
                        0.0
                    };
                }
            }
        }
        for co in 0..cout {
            let mut acc = [b[co]; TILE];
            for kt in 0..3usize {
                for ci in 0..cin {
                    let wl = &win[kt * cin + ci];
                    let wbase = (co * cin + ci) * 25;
                    for kf in 0..5usize {
                        let wv = w[wbase + kf * 5 + kt];
                        for j in 0..TILE {
                            acc[j] += wv * wl[j + kf];
                        }
                    }
                }
            }
            out[co * N_FREQ + base as usize..co * N_FREQ + base as usize + TILE]
                .copy_from_slice(&acc);
        }
    }
}

/// `ln(x + 1e-8)` — named helper so the feature formula reads like the spec.
trait LnEps {
    fn ln_eps(self) -> f32;
}
impl LnEps for f32 {
    #[inline]
    fn ln_eps(self) -> f32 {
        (self + 1e-8).ln()
    }
}
