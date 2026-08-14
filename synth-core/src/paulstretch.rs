//! Extreme time-stretch smear — the PaulStretch algorithm, live.
//!
//! Paul Nasca's insight: to stretch by 8× you do **not** need to preserve phase
//! relationships. Take a long window, keep its magnitude spectrum, throw the
//! phases away and replace them with noise, resynthesise, and overlap-add at a
//! larger hop than you read with. Because the phases are random the frames can't
//! cancel or comb; the result is the famous smooth, glassy smear rather than the
//! metallic flanging a phase vocoder gives at extreme ratios.
//!
//! ```text
//!   input ──▶ capture ring ──▶ [read hop = out_hop / stretch]
//!                                   │
//!                        long Hann window → FFT
//!                                   │
//!                   |X| kept ──▶ smoothing ──▶ spectral pitch shift
//!                                   │
//!                     phase := random  (blended toward the original by Tonal)
//!                                   │
//!                        iFFT → window → overlap-add ──▶ out
//! ```
//!
//! ## Live vs Freeze
//! Stretching by N× consumes input N times slower than it produces output, so a
//! real-time stretcher has to decide what to do when the read head falls too far
//! behind. Two honest answers, both provided:
//!
//! - **Live** — the read head trails the write head and, when it would fall off
//!   the end of the ring, jumps forward to half a buffer behind. You hear a
//!   continuous smear of the recent past with an occasional skip. Good for pads
//!   under a live source.
//! - **Freeze** — capture stops and the read head circles the last `Length`
//!   seconds forever. Sing one note, freeze it, and it becomes an infinite pad.
//!
//! ## Latency
//! Reported as zero on purpose: a stretched output is not sample-aligned with its
//! input in any meaningful sense, so PDC has nothing sensible to compensate.
//! Don't use this on a parallel bus expecting phase coherence.
//!
//! **RT-safe:** FFT plans for *every* selectable window size are built in
//! [`PaulStretch::new`], so changing `Window` at runtime allocates nothing.
//! [`process`](PaulStretch::process) never allocates, locks, or panics.

use crate::dsp_blocks::Xorshift;
use crate::spectral::smooth_proportional;
use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Selectable window sizes. At 48 kHz: 85 ms … 1.37 s. Longer = smoother and
/// more washed out; shorter keeps more rhythm and transient identity.
pub const WINDOW_SIZES: [usize; 5] = [4096, 8192, 16384, 32768, 65536];
pub const MAX_WINDOW: usize = 65536;

/// Capture ring length in seconds.
pub const BUFFER_SECONDS: f32 = 12.0;

/// Make-up for incoherent overlap-add. With randomised phase, overlapping frames
/// add in power rather than amplitude, so the COLA normalisation that's correct
/// for a phase-coherent STFT leaves the output ~√2 quiet. Applied in proportion
/// to how much the phase was actually randomised.
const INCOHERENT_MAKEUP: f32 = std::f32::consts::SQRT_2;

#[derive(Clone, Copy)]
pub struct StretchParams {
    /// Time-stretch factor (1 = none, 50 = glacial).
    pub stretch: f32,
    /// Index into [`WINDOW_SIZES`].
    pub window: usize,
    /// 0 = fully randomised phase (classic PaulStretch smear), 1 = keep the
    /// analysed phase (a plain, more tonal slow-down).
    pub tonal: f32,
    /// Spectral-envelope smoothing (0 = none, 1 = heavy) — blurs timbre.
    pub smooth: f32,
    /// Spectral pitch shift in semitones.
    pub pitch_semi: f32,
    /// Stop capturing and circle the last `length_s` seconds forever.
    pub freeze: bool,
    /// Length of the frozen region in seconds.
    pub length_s: f32,
    pub mix: f32,
    pub output_lin: f32,
    pub bypassed: bool,
}

impl Default for StretchParams {
    fn default() -> Self {
        Self {
            stretch: 8.0,
            window: 2,
            tonal: 0.0,
            smooth: 0.0,
            pitch_semi: 0.0,
            freeze: false,
            length_s: 6.0,
            mix: 1.0,
            output_lin: 1.0,
            bypassed: false,
        }
    }
}

pub struct PaulStretch {
    sr: f32,
    cap: usize,
    buf_l: Box<[f32]>,
    buf_r: Box<[f32]>,
    write: usize,
    /// Fractional read position (f64 — at 50× stretch the per-frame increment is
    /// tiny and f32 would quantise it).
    read: f64,
    /// One forward/inverse plan + Hann window per selectable size.
    fwd: Vec<Arc<dyn RealToComplex<f32>>>,
    inv: Vec<Arc<dyn ComplexToReal<f32>>>,
    windows: Vec<Box<[f32]>>,
    scratch_fwd: Box<[Complex<f32>]>,
    scratch_inv: Box<[Complex<f32>]>,
    time_l: Box<[f32]>,
    time_r: Box<[f32]>,
    spec_l: Box<[Complex<f32>]>,
    spec_r: Box<[Complex<f32>]>,
    mag_l: Box<[f32]>,
    mag_r: Box<[f32]>,
    smooth_l: Box<[f32]>,
    smooth_r: Box<[f32]>,
    /// Prefix-sum scratch for the O(n) magnitude smoother.
    smooth_prefix: Box<[f64]>,
    accum_l: Box<[f32]>,
    accum_r: Box<[f32]>,
    /// How far into the current hop the drain has got. Zero means the previous
    /// hop is spent and the next frame is due — one counter, not a
    /// `ready`/`position` pair that has to be kept complementary by hand.
    out_pos: usize,
    rng: Xorshift,
    /// Window size in use — a change forces the accumulator to be rebuilt.
    cur_window: usize,
    was_frozen: bool,
    /// Freeze as actually applied this block (see `process`) — `render_frame`
    /// must agree with the capture path about whether the ring is frozen.
    frozen: bool,
    /// Write position captured when Freeze engaged (end of the frozen region).
    anchor: usize,
    /// True once the ring holds at least one full window of audio.
    primed_samples: usize,
    /// Whether the read head has been placed relative to the write head yet.
    read_seeded: bool,
}

impl PaulStretch {
    pub fn new(sr: f32) -> Self {
        let cap = ((sr * BUFFER_SECONDS) as usize).max(MAX_WINDOW * 2);
        let mut planner = RealFftPlanner::<f32>::new();
        let mut fwd = Vec::with_capacity(WINDOW_SIZES.len());
        let mut inv = Vec::with_capacity(WINDOW_SIZES.len());
        let mut windows = Vec::with_capacity(WINDOW_SIZES.len());
        for &n in WINDOW_SIZES.iter() {
            fwd.push(planner.plan_fft_forward(n));
            inv.push(planner.plan_fft_inverse(n));
            windows.push(
                (0..n)
                    .map(|k| 0.5 - 0.5 * (core::f32::consts::TAU * k as f32 / n as f32).cos())
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            );
        }
        // Scratch sized for the largest plan so no size change ever allocates.
        let scratch_fwd = fwd[WINDOW_SIZES.len() - 1]
            .make_scratch_vec()
            .into_boxed_slice();
        let scratch_inv = inv[WINDOW_SIZES.len() - 1]
            .make_scratch_vec()
            .into_boxed_slice();
        let half = MAX_WINDOW / 2 + 1;
        Self {
            sr,
            cap,
            buf_l: vec![0.0; cap].into_boxed_slice(),
            buf_r: vec![0.0; cap].into_boxed_slice(),
            write: 0,
            read: 0.0,
            fwd,
            inv,
            windows,
            scratch_fwd,
            scratch_inv,
            time_l: vec![0.0; MAX_WINDOW].into_boxed_slice(),
            time_r: vec![0.0; MAX_WINDOW].into_boxed_slice(),
            spec_l: vec![Complex::new(0.0, 0.0); half].into_boxed_slice(),
            spec_r: vec![Complex::new(0.0, 0.0); half].into_boxed_slice(),
            mag_l: vec![0.0; half].into_boxed_slice(),
            mag_r: vec![0.0; half].into_boxed_slice(),
            smooth_l: vec![0.0; half].into_boxed_slice(),
            smooth_r: vec![0.0; half].into_boxed_slice(),
            smooth_prefix: vec![0.0; half + 1].into_boxed_slice(),
            accum_l: vec![0.0; MAX_WINDOW * 2].into_boxed_slice(),
            accum_r: vec![0.0; MAX_WINDOW * 2].into_boxed_slice(),
            out_pos: 0,
            rng: Xorshift::new(0x9E37_79B9),
            cur_window: usize::MAX,
            was_frozen: false,
            frozen: false,
            anchor: 0,
            primed_samples: 0,
            read_seeded: false,
        }
    }

    pub fn reset(&mut self) {
        self.buf_l.fill(0.0);
        self.buf_r.fill(0.0);
        self.accum_l.fill(0.0);
        self.accum_r.fill(0.0);
        self.write = 0;
        self.read = 0.0;
        self.out_pos = 0;
        self.cur_window = usize::MAX;
        self.was_frozen = false;
        self.frozen = false;
        self.primed_samples = 0;
        self.read_seeded = false;
    }

    /// Read position as a fraction of the ring — for a GUI playhead.
    #[inline]
    pub fn read_phase(&self) -> f32 {
        (self.read as f32 / self.cap as f32).rem_euclid(1.0)
    }
    #[inline]
    pub fn write_phase(&self) -> f32 {
        self.write as f32 / self.cap as f32
    }

    /// Largest safe distance behind the write head, in samples. The ring only
    /// holds what has been written into it, so seating the read head half a ring
    /// back on a freshly-inserted plugin would read pure silence — and at 8×
    /// stretch it would take ~30 s of playback to crawl out of it.
    #[inline]
    fn max_back(&self, window: usize) -> f64 {
        let available = self.primed_samples.min(self.cap) as f64;
        (available - window as f64).max(window as f64)
    }

    #[inline]
    fn ring(buf: &[f32], cap: usize, idx: isize) -> f32 {
        buf[idx.rem_euclid(cap as isize) as usize]
    }

    /// Linear interpolation of the smoothed magnitude at a fractional bin.
    #[inline]
    fn mag_at(mag: &[f32], bin: f32) -> f32 {
        if bin < 0.0 {
            return 0.0;
        }
        let i = bin.floor() as usize;
        if i + 1 >= mag.len() {
            return 0.0;
        }
        let f = bin - i as f32;
        mag[i] * (1.0 - f) + mag[i + 1] * f
    }

    /// Render one frame into the overlap-add accumulator and advance the read
    /// head by `hop / stretch` input samples.
    fn render_frame(&mut self, p: &StretchParams) {
        let wi = p.window.min(WINDOW_SIZES.len() - 1);
        let n = WINDOW_SIZES[wi];
        let hop = n / 2;
        let half = n / 2;

        // A window-size change invalidates the accumulator's overlap state.
        if self.cur_window != n {
            self.accum_l.fill(0.0);
            self.accum_r.fill(0.0);
            self.out_pos = 0;
            self.cur_window = n;
        }

        // ---- Analysis window out of the ring ---------------------------
        let base = self.read.floor() as isize;
        let win = &self.windows[wi];
        for k in 0..n {
            let idx = base + k as isize;
            self.time_l[k] = Self::ring(&self.buf_l, self.cap, idx) * win[k];
            self.time_r[k] = Self::ring(&self.buf_r, self.cap, idx) * win[k];
        }
        let _ = self.fwd[wi].process_with_scratch(
            &mut self.time_l[..n],
            &mut self.spec_l[..half + 1],
            &mut self.scratch_fwd,
        );
        let _ = self.fwd[wi].process_with_scratch(
            &mut self.time_r[..n],
            &mut self.spec_r[..half + 1],
            &mut self.scratch_fwd,
        );

        // ---- Magnitudes (+ optional envelope smoothing) -----------------
        for b in 0..=half {
            self.mag_l[b] = self.spec_l[b].norm();
            self.mag_r[b] = self.spec_r[b].norm();
        }
        let smooth = p.smooth.clamp(0.0, 1.0);
        if smooth > 0.001 {
            // Frequency-proportional moving average — a fixed width would erase
            // the low end while barely touching the top.
            let frac = 0.02 + smooth * 0.25;
            smooth_proportional(
                &self.mag_l[..=half],
                &mut self.smooth_l[..=half],
                frac,
                &mut self.smooth_prefix,
            );
            smooth_proportional(
                &self.mag_r[..=half],
                &mut self.smooth_r[..=half],
                frac,
                &mut self.smooth_prefix,
            );
            self.mag_l[..=half].copy_from_slice(&self.smooth_l[..=half]);
            self.mag_r[..=half].copy_from_slice(&self.smooth_r[..=half]);
        }

        // ---- Resynthesis: magnitude kept, phase randomised --------------
        let ratio = (p.pitch_semi / 12.0).exp2().clamp(0.03, 32.0);
        let tonal = p.tonal.clamp(0.0, 1.0);
        let shifting = (ratio - 1.0).abs() > 1e-4;
        for b in 0..=half {
            let (m_l, m_r) = if shifting {
                let src = b as f32 / ratio;
                (
                    Self::mag_at(&self.mag_l[..=half], src),
                    Self::mag_at(&self.mag_r[..=half], src),
                )
            } else {
                (self.mag_l[b], self.mag_r[b])
            };
            // Random phase, pulled back toward the analysed phase by Tonal. The
            // two channels get the SAME random phase per bin so the stereo image
            // survives (independent phases collapse it into decorrelated mush).
            let rnd = self.rng.next_f32() * core::f32::consts::TAU;
            let ph_l = self.spec_l[b].arg();
            let ph_r = self.spec_r[b].arg();
            let a_l = ph_l * tonal + rnd * (1.0 - tonal);
            let a_r = ph_r * tonal + rnd * (1.0 - tonal);
            self.spec_l[b] = Complex::from_polar(m_l, a_l);
            self.spec_r[b] = Complex::from_polar(m_r, a_r);
        }
        // A clean real inverse needs DC and Nyquist purely real.
        self.spec_l[0].im = 0.0;
        self.spec_r[0].im = 0.0;
        self.spec_l[half].im = 0.0;
        self.spec_r[half].im = 0.0;

        let _ = self.inv[wi].process_with_scratch(
            &mut self.spec_l[..half + 1],
            &mut self.time_l[..n],
            &mut self.scratch_inv,
        );
        let _ = self.inv[wi].process_with_scratch(
            &mut self.spec_r[..half + 1],
            &mut self.time_r[..n],
            &mut self.scratch_inv,
        );

        // ---- Windowed overlap-add --------------------------------------
        // Hann analysis × Hann synthesis at 50 % overlap sums to
        // mean(Hann²)·osamp = 0.75, and realfft's inverse is unnormalised (×n).
        let cola = 0.375 * (n as f32 / hop as f32);
        let mut scale = 1.0 / (n as f32 * cola);
        scale *= 1.0 + (INCOHERENT_MAKEUP - 1.0) * (1.0 - tonal);
        for k in 0..n {
            self.accum_l[k] += self.time_l[k] * win[k] * scale;
            self.accum_r[k] += self.time_r[k] * win[k] * scale;
        }
        self.out_pos = 0;

        // ---- Advance the read head -------------------------------------
        let stretch = p.stretch.max(0.05) as f64;
        self.read += hop as f64 / stretch;

        // Keep the read head inside a sane region.
        let cap = self.cap as f64;
        if self.frozen {
            // Circle the frozen region: [anchor − length, anchor). Clamped to
            // what the ring holds — a longer Length than the take would wrap the
            // read head into never-written samples.
            let available = self.primed_samples.min(self.cap) as f64;
            let len = ((p.length_s.clamp(0.05, BUFFER_SECONDS) * self.sr) as f64).min(available);
            let end = self.anchor as f64;
            let start = end - len;
            if self.read >= end || self.read < start - 1.0 {
                self.read = start;
            }
        } else {
            // Live: stay behind the write head. Stretching consumes input slower
            // than it emits, so the gap grows without bound — when it reaches
            // most of the ring, skip forward (a jump is audible but honest;
            // the alternative is reading samples that were overwritten).
            let w = self.write as f64;
            let mut gap = w - self.read;
            if gap < 0.0 {
                gap += cap;
            }
            let guard = n as f64 + 64.0;
            if gap > cap * 0.85 {
                // Fallen too far behind — skip forward into fresher audio, but
                // never past what has actually been captured.
                self.read = w - (cap * 0.5).min(self.max_back(n));
            } else if gap < guard {
                // Caught up with (or overtaken) the write head: back off so a
                // whole window of already-written audio sits ahead of us.
                self.read = w - guard * 2.0;
            }
        }
    }

    /// Process one block. `write_r` may be empty for a mono track.
    pub fn process(
        &mut self,
        read_l: &[f32],
        read_r: &[f32],
        write_l: &mut [f32],
        write_r: &mut [f32],
        p: &StretchParams,
    ) {
        let n_frames = read_l.len().min(write_l.len());
        if p.bypassed {
            write_l[..n_frames].copy_from_slice(&read_l[..n_frames]);
            if !write_r.is_empty() {
                let m = n_frames.min(write_r.len()).min(read_r.len());
                write_r[..m].copy_from_slice(&read_r[..m]);
            }
            return;
        }

        let mix = p.mix.clamp(0.0, 1.0);
        let n_window = WINDOW_SIZES[p.window.min(WINDOW_SIZES.len() - 1)];

        // Freeze can only loop material that exists. A fresh instance recalling
        // the "Freeze Pad" preset has an empty ring, and gating the capture also
        // gates the priming counter, so Freeze used to lock the plugin onto
        // silence permanently. It is therefore a no-op — capture continues, the
        // plugin runs live — until the region it is asked to loop (`Length`, and
        // at least two windows so a frame always has neighbours) is actually in
        // the ring.
        let region = ((p.length_s.clamp(0.05, BUFFER_SECONDS) * self.sr) as usize)
            .max(n_window * 2)
            .min(self.cap);
        let freeze = p.freeze && self.primed_samples >= region;
        self.frozen = freeze;

        // Freeze edge: remember where the recording stopped and start the loop
        // one region-length behind it.
        if freeze && !self.was_frozen {
            self.anchor = self.write;
            let len = (p.length_s.clamp(0.05, BUFFER_SECONDS) * self.sr) as f64;
            self.read = self.anchor as f64 - len.min(self.max_back(n_window));
        }
        if !freeze && self.was_frozen {
            // Back to live: re-seat the read head behind the write head — again,
            // bounded by what the ring actually holds.
            self.read = self.write as f64 - (self.cap as f64 * 0.5).min(self.max_back(n_window));
        }
        self.was_frozen = freeze;

        for i in 0..n_frames {
            let dry_l = read_l[i];
            let dry_r = if i < read_r.len() { read_r[i] } else { dry_l };

            if !freeze {
                self.buf_l[self.write] = dry_l;
                self.buf_r[self.write] = dry_r;
                self.write += 1;
                if self.write >= self.cap {
                    self.write = 0;
                }
                self.primed_samples = self.primed_samples.saturating_add(1);
            }

            // Until the ring holds a full window there is nothing to stretch —
            // emit dry so the plugin doesn't start with a burst of noise.
            if self.primed_samples < n_window {
                write_l[i] = dry_l * p.output_lin;
                if !write_r.is_empty() && i < write_r.len() {
                    write_r[i] = dry_r * p.output_lin;
                }
                continue;
            }

            if !self.read_seeded {
                // First frame: read the window that was just captured, not a
                // position half a ring back where the buffer is still empty.
                self.read = self.write as f64 - n_window as f64;
                self.read_seeded = true;
            }
            // out_pos == 0 means the last hop is spent, so a frame is due.
            if self.out_pos == 0 {
                self.render_frame(p);
            }

            let wet_l = self.accum_l[self.out_pos];
            let wet_r = self.accum_r[self.out_pos];
            self.out_pos += 1;
            let n = self.cur_window;
            let hop = n / 2;
            if self.out_pos == hop {
                // Frame drained: shift the accumulator down by one hop.
                self.accum_l.copy_within(hop..hop + n, 0);
                self.accum_r.copy_within(hop..hop + n, 0);
                for k in n..n + hop {
                    self.accum_l[k] = 0.0;
                    self.accum_r[k] = 0.0;
                }
                self.out_pos = 0;
            }

            write_l[i] = (dry_l * (1.0 - mix) + wet_l * mix) * p.output_lin;
            if !write_r.is_empty() && i < write_r.len() {
                write_r[i] = (dry_r * (1.0 - mix) + wet_r * mix) * p.output_lin;
            }
        }
    }
}
