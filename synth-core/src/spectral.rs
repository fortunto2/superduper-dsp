//! Shared STFT overlap-add scaffolding for spectral effects.
//!
//! A streaming short-time Fourier transform processor: it owns the Hann
//! window, the realfft forward/inverse plans, the input ring(s), the synthesis
//! overlap-add accumulator, and the COLA normalisation — everything except the
//! actual per-frame spectral operation, which the caller supplies as a
//! callback. Both SuperDuper Pitch (phase-vocoder pitch shift, 1 input) and
//! SuperDuper Vocoder (spectral cross-synthesis, 2 inputs = modulator + carrier)
//! build on this, so the STFT scaffolding lives in exactly one place.
//!
//! One instance = one channel. Hold `[StftProcessor; 2]` for stereo.
//!
//! **RT-safe:** all buffers + FFT plans are allocated in [`StftProcessor::new`];
//! [`process_sample`](StftProcessor::process_sample) never allocates. Latency =
//! `N − hop` (reported by the caller via the CLAP latency extension).
//!
//! ## Normalisation
//! Hann analysis × Hann synthesis at hop `N/osamp` overlap-adds to a constant
//! `mean(Hann²)·osamp = 0.375·osamp` (= 1.5 at 75 % overlap), and realfft's
//! inverse is unnormalised (fwd·inv = N). The processor divides both out so a
//! pass-through op reconstructs the input at unity — the COLA lesson from the
//! pitch-shifter fix, applied once here.

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub struct StftProcessor {
    n: usize,
    hop: usize,
    half: usize,
    latency: usize,
    inputs: usize,
    cola_scale: f32,
    /// Expected per-hop phase advance factor `2π·hop/N` (for phase-vocoder ops).
    expct: f32,
    /// Hz per FFT bin (`sr/N`).
    freq_per_bin: f32,
    window: Box<[f32]>,
    fwd: Arc<dyn RealToComplex<f32>>,
    inv: Arc<dyn ComplexToReal<f32>>,
    scratch_fwd: Box<[Complex<f32>]>,
    scratch_inv: Box<[Complex<f32>]>,
    fft_time: Box<[f32]>,
    /// One analysis input ring per input stream (`inputs × N`).
    in_fifo: Vec<Box<[f32]>>,
    /// Analysis spectrum scratch, one per input (`inputs × (half+1)`).
    ana: Vec<Box<[Complex<f32>]>>,
    /// Synthesis spectrum scratch (`half+1`).
    syn: Box<[Complex<f32>]>,
    out_accum: Box<[f32]>,
    out_fifo: Box<[f32]>,
    rover: usize,
}

impl StftProcessor {
    /// `inputs` = number of analysis streams (1 for pitch, 2 for a vocoder's
    /// modulator + carrier).
    pub fn new(sr: f32, n: usize, hop: usize, inputs: usize) -> Self {
        assert!(inputs >= 1 && inputs <= 2, "StftProcessor supports 1..=2 inputs");
        let half = n / 2;
        let latency = n - hop;
        let osamp = n as f32 / hop as f32;
        let cola = 0.375 * osamp; // mean(Hann²)=3/8, × overlap factor
        let cola_scale = 1.0 / (n as f32 * cola);
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(n);
        let inv = planner.plan_fft_inverse(n);
        let scratch_fwd = fwd.make_scratch_vec().into_boxed_slice();
        let scratch_inv = inv.make_scratch_vec().into_boxed_slice();
        let window: Box<[f32]> = (0..n)
            .map(|k| 0.5 - 0.5 * (core::f32::consts::TAU * k as f32 / n as f32).cos())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let in_fifo = (0..inputs).map(|_| vec![0.0; n].into_boxed_slice()).collect();
        let ana = (0..inputs)
            .map(|_| vec![Complex::new(0.0, 0.0); half + 1].into_boxed_slice())
            .collect();
        Self {
            n,
            hop,
            half,
            latency,
            inputs,
            cola_scale,
            expct: core::f32::consts::TAU * hop as f32 / n as f32,
            freq_per_bin: sr / n as f32,
            window,
            fwd,
            inv,
            scratch_fwd,
            scratch_inv,
            fft_time: vec![0.0; n].into_boxed_slice(),
            in_fifo,
            ana,
            syn: vec![Complex::new(0.0, 0.0); half + 1].into_boxed_slice(),
            out_accum: vec![0.0; 2 * n].into_boxed_slice(),
            out_fifo: vec![0.0; n].into_boxed_slice(),
            rover: latency,
        }
    }

    #[inline]
    pub fn latency(&self) -> usize {
        self.latency
    }
    #[inline]
    pub fn n(&self) -> usize {
        self.n
    }
    #[inline]
    pub fn hop(&self) -> usize {
        self.hop
    }
    #[inline]
    pub fn half(&self) -> usize {
        self.half
    }
    #[inline]
    pub fn expct(&self) -> f32 {
        self.expct
    }
    #[inline]
    pub fn freq_per_bin(&self) -> f32 {
        self.freq_per_bin
    }
    #[inline]
    pub fn osamp(&self) -> f32 {
        self.n as f32 / self.hop as f32
    }
    #[inline]
    pub fn window(&self) -> &[f32] {
        &self.window
    }

    pub fn reset(&mut self) {
        for r in self.in_fifo.iter_mut() {
            r.fill(0.0);
        }
        self.out_accum.fill(0.0);
        self.out_fifo.fill(0.0);
        self.rover = self.latency;
    }

    /// Push one sample per input stream, return the `latency`-delayed output.
    /// On each hop boundary the analysis FFT(s) run, `frame_op` fills the
    /// synthesis spectrum from the analysis spectra, and the iFFT + OLA runs.
    /// `frame_op(analysis, synthesis)` — `analysis[i]` is input `i`'s spectrum
    /// (`half+1` bins), `synthesis` is written by the callback.
    #[inline]
    pub fn process_sample<F>(&mut self, inputs: &[f32], mut frame_op: F) -> f32
    where
        F: FnMut(&[&[Complex<f32>]], &mut [Complex<f32>]),
    {
        for s in 0..self.inputs {
            self.in_fifo[s][self.rover] = inputs[s];
        }
        let out = self.out_fifo[self.rover - self.latency];
        self.rover += 1;
        if self.rover >= self.n {
            self.rover = self.latency;
            self.run_frame(&mut frame_op);
        }
        out
    }

    fn run_frame<F>(&mut self, frame_op: &mut F)
    where
        F: FnMut(&[&[Complex<f32>]], &mut [Complex<f32>]),
    {
        let n = self.n;
        // Analysis FFT for each input stream.
        for s in 0..self.inputs {
            for k in 0..n {
                self.fft_time[k] = self.in_fifo[s][k] * self.window[k];
            }
            let _ = self.fwd.process_with_scratch(
                &mut self.fft_time,
                &mut self.ana[s],
                &mut self.scratch_fwd,
            );
        }

        // Per-frame spectral operation (split borrows: ana immut, syn mut).
        {
            let ana = &self.ana;
            let syn = &mut self.syn;
            if self.inputs == 1 {
                frame_op(&[&ana[0][..]], &mut syn[..]);
            } else {
                frame_op(&[&ana[0][..], &ana[1][..]], &mut syn[..]);
            }
        }
        // A clean real inverse wants DC + Nyquist purely real.
        self.syn[0].im = 0.0;
        self.syn[self.half].im = 0.0;

        // iFFT + window + overlap-add.
        let _ =
            self.inv
                .process_with_scratch(&mut self.syn, &mut self.fft_time, &mut self.scratch_inv);
        for k in 0..n {
            self.out_accum[k] += self.window[k] * self.fft_time[k] * self.cola_scale;
        }
        for k in 0..self.hop {
            self.out_fifo[k] = self.out_accum[k];
        }
        self.out_accum.copy_within(self.hop..self.hop + n, 0);
        for k in 0..n {
            self.out_accum[k + n] = 0.0;
        }
        for s in 0..self.inputs {
            self.in_fifo[s].copy_within(self.hop..self.hop + self.latency, 0);
        }
    }
}

/// Frequency-proportional moving average over a magnitude spectrum.
///
/// A fixed smoothing width can't serve a whole spectrum: enough averaging to
/// flatten the harmonic comb at 2.5 kHz will also merge F1 and F2 of an open
/// vowel down at 700-1100 Hz. The half-width therefore grows with the bin index
/// (`frac` of it) — constant-Q in spirit.
///
/// **O(n), not O(n·width).** Re-summing each window from scratch is quadratic in
/// disguise, because the window itself grows with the bin index: at a 32768-bin
/// spectrum with `frac = 0.13` that is ~1.4e8 additions *per channel per frame*,
/// which overruns an audio deadline by orders of magnitude. A prefix sum makes
/// every window a single subtraction. `prefix` is caller-owned scratch of at
/// least `src.len() + 1`; it is `f64` because differencing two large f32 partial
/// sums loses the precision that narrow low-bin windows depend on.
pub fn smooth_proportional(src: &[f32], dst: &mut [f32], frac: f32, prefix: &mut [f64]) {
    let n = src.len().min(dst.len()).min(prefix.len().saturating_sub(1));
    if n == 0 {
        return;
    }
    prefix[0] = 0.0;
    for k in 0..n {
        prefix[k + 1] = prefix[k] + src[k] as f64;
    }
    for k in 0..n {
        let w = ((k as f32 * frac) as usize).max(1);
        let lo = k.saturating_sub(w);
        let hi = (k + w).min(n - 1);
        dst[k] = ((prefix[hi + 1] - prefix[lo]) / (hi - lo + 1) as f64) as f32;
    }
}
