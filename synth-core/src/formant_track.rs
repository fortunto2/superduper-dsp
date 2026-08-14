//! Live formant tracker — estimates F1/F2/F3 of a signal at hop rate so a
//! *different* sound can be articulated with them.
//!
//! This is the analysis half of "sing into it, the kubyz speaks": the voice
//! goes into the tracker, the tracker reports the three vocal-tract
//! resonances, and [`crate::formant::Formant`] imposes them on whatever is on
//! the main input. It is deliberately NOT a vocoder — a vocoder copies the
//! whole spectral envelope band-by-band (intelligible, robotic); three
//! resonances copy only what a mouth actually does (singing, instrumental).
//!
//! ## Method
//! Per hop: Hann-windowed FFT → magnitude → **frequency-proportional
//! smoothing** (a constant-Q-ish moving average: narrow at the bottom so F1
//! and F2 don't merge on an open vowel, wide up top so the harmonic comb
//! flattens) → peak-pick inside a per-formant search range with parabolic
//! sub-bin interpolation → monotonicity fix-up (F1 < F2 < F3) → one-pole glide.
//!
//! Below the gate the estimate **freezes** rather than collapsing, so a
//! breath or a pause holds the last vowel instead of snapping to noise.
//!
//! **RT-safe:** everything is allocated in [`FormantTracker::new`];
//! [`push`](FormantTracker::push) never allocates. The tracker only *reads* —
//! it adds no latency to the audio path (the formant values themselves lag by
//! at most one hop, 5.3 ms @ 48 kHz).

use crate::spectral::smooth_proportional;
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

/// Analysis window. 1024 @ 48 kHz = 21 ms — long enough to resolve a male F1
/// (~700 Hz), short enough to follow a sung vowel change.
pub const TRACK_N: usize = 1024;
/// Hop between analyses — 5.3 ms @ 48 kHz.
pub const TRACK_HOP: usize = 256;

const HALF: usize = TRACK_N / 2;

/// Per-formant search range in Hz, wide enough for both male and female
/// tracts (Peterson-Barney extremes plus headroom).
pub const SEARCH_HZ: [(f32, f32); 3] = [(180.0, 1150.0), (550.0, 2900.0), (1500.0, 4300.0)];

/// Minimum spacing ratio F2/F1 and F3/F2 — stops the picker from returning the
/// same resonance twice when two search ranges overlap.
const MIN_RATIO: [f32; 2] = [1.15, 1.10];

/// Fractional smoothing width: half-width in bins ≈ `bin · Q_FRACTION`.
const Q_FRACTION: f32 = 0.11;

/// Pre-emphasis coefficient (`y[n] = x[n] − α·x[n−1]`, ≈ +6 dB/oct).
///
/// Without this the picker is unusable: a glottal source rolls off ~6 dB/oct,
/// so on a closed vowel like /i/ (F1 270, F2 2290) the *skirt* of F1 at 600 Hz
/// is taller than the F2 peak itself and the F2 search returns 586 Hz. Tilting
/// the spectrum up before peak-picking puts the three resonances on a
/// comparable footing — standard practice in speech formant analysis.
const PRE_EMPH: f32 = 0.97;

/// Neutral formants used before anything has been tracked (schwa-ish).
const NEUTRAL: [f32; 3] = [600.0, 1200.0, 2500.0];

pub struct FormantTracker {
    sr: f32,
    fwd: Arc<dyn RealToComplex<f32>>,
    scratch: Box<[Complex<f32>]>,
    window: Box<[f32]>,
    /// Circular input history, `TRACK_N` long.
    ring: Box<[f32]>,
    write: usize,
    since_hop: usize,
    fft_time: Box<[f32]>,
    spec: Box<[Complex<f32>]>,
    mag: Box<[f32]>,
    /// Smoothed spectral envelope — what the peak picker reads (also the
    /// natural thing for a GUI to draw).
    env: Box<[f32]>,
    /// Prefix-sum scratch for the O(n) envelope smoother.
    prefix: Box<[f64]>,
    /// Glided output values.
    smoothed: [f32; 3],
    /// Latest raw per-hop estimate (pre-glide).
    raw: [f32; 3],
    level_db: f32,
    active: bool,
}

impl FormantTracker {
    pub fn new(sr: f32) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(TRACK_N);
        let scratch = fwd.make_scratch_vec().into_boxed_slice();
        let window: Box<[f32]> = (0..TRACK_N)
            .map(|k| 0.5 - 0.5 * (core::f32::consts::TAU * k as f32 / TRACK_N as f32).cos())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            sr,
            fwd,
            scratch,
            window,
            ring: vec![0.0; TRACK_N].into_boxed_slice(),
            write: 0,
            since_hop: 0,
            fft_time: vec![0.0; TRACK_N].into_boxed_slice(),
            spec: vec![Complex::new(0.0, 0.0); HALF + 1].into_boxed_slice(),
            mag: vec![0.0; HALF + 1].into_boxed_slice(),
            env: vec![0.0; HALF + 1].into_boxed_slice(),
            prefix: vec![0.0; HALF + 2].into_boxed_slice(),
            smoothed: NEUTRAL,
            raw: NEUTRAL,
            level_db: -120.0,
            active: false,
        }
    }

    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.env.fill(0.0);
        self.mag.fill(0.0);
        self.write = 0;
        self.since_hop = 0;
        self.smoothed = NEUTRAL;
        self.raw = NEUTRAL;
        self.level_db = -120.0;
        self.active = false;
    }

    /// Glided formant estimate — F1, F2, F3 in Hz.
    #[inline]
    pub fn formants(&self) -> [f32; 3] {
        self.smoothed
    }

    /// True while the input is above the gate (i.e. the estimate is live
    /// rather than frozen).
    #[inline]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Frame level in dBFS — drives the gate and is worth showing in a GUI.
    #[inline]
    pub fn level_db(&self) -> f32 {
        self.level_db
    }

    /// Feed one sample of the signal being tracked (mono — sum stereo first).
    ///
    /// `glide_ms` is the one-pole time constant of the formant motion,
    /// `gate_db` the level below which tracking freezes. Returns `true` on the
    /// samples where a new analysis frame was computed.
    #[inline]
    pub fn push(&mut self, x: f32, glide_ms: f32, gate_db: f32) -> bool {
        self.ring[self.write] = x;
        self.write = if self.write + 1 == TRACK_N { 0 } else { self.write + 1 };
        self.since_hop += 1;
        if self.since_hop < TRACK_HOP {
            return false;
        }
        self.since_hop = 0;
        self.run_frame(glide_ms, gate_db);
        true
    }

    fn run_frame(&mut self, glide_ms: f32, gate_db: f32) {
        // Oldest sample sits at `write` (we just wrapped past it). Pre-emphasis
        // runs on the raw history, the gate measures the *newest* hop only —
        // gating on the whole window would keep the analysis alive for four
        // frames after the input stopped, and those half-empty windows shift
        // the picked peaks (the estimate must freeze cleanly instead).
        let mut energy = 0.0f32;
        let mut prev = self.ring[self.write];
        for k in 0..TRACK_N {
            let idx = {
                let i = self.write + k;
                if i >= TRACK_N { i - TRACK_N } else { i }
            };
            let s = self.ring[idx];
            if k >= TRACK_N - TRACK_HOP {
                energy += s * s;
            }
            self.fft_time[k] = (s - PRE_EMPH * prev) * self.window[k];
            prev = s;
        }
        let rms = (energy / TRACK_HOP as f32).sqrt();
        self.level_db = 20.0 * (rms + 1e-12).log10();
        self.active = self.level_db > gate_db;

        // Below the gate: hold the last vowel. A pause should not snap the
        // articulation to whatever noise floor is present.
        if !self.active {
            return;
        }

        let _ = self
            .fwd
            .process_with_scratch(&mut self.fft_time, &mut self.spec, &mut self.scratch);
        for k in 0..=HALF {
            self.mag[k] = self.spec[k].norm();
        }
        smooth_proportional(&self.mag, &mut self.env, Q_FRACTION, &mut self.prefix);

        // ---- Peak-pick each formant inside its range ----------------------
        let hz_per_bin = self.sr / TRACK_N as f32;
        let mut prev_hz = 0.0f32;
        let mut out = [0.0f32; 3];
        for i in 0..3 {
            let (lo_hz, hi_hz) = SEARCH_HZ[i];
            // Push the low edge up so we can't re-find the previous formant.
            let lo_hz = if i == 0 {
                lo_hz
            } else {
                lo_hz.max(prev_hz * MIN_RATIO[i - 1])
            };
            if lo_hz >= hi_hz {
                // Ranges collapsed (very high F1) — fall back to a fixed step.
                out[i] = (prev_hz * MIN_RATIO[i - 1]).min(self.sr * 0.45);
                prev_hz = out[i];
                continue;
            }
            let lo_bin = (lo_hz / hz_per_bin).ceil().max(1.0) as usize;
            let hi_bin = ((hi_hz / hz_per_bin).floor() as usize).min(HALF - 1);
            let hz = match pick_peak(&self.env, lo_bin, hi_bin) {
                Some(bin) => bin * hz_per_bin,
                // Nothing resolvable in range — keep the previous glide target.
                None => self.raw[i],
            };
            out[i] = hz;
            prev_hz = hz;
        }
        self.raw = out;

        // ---- Glide toward the new estimate at hop rate ---------------------
        let dt = TRACK_HOP as f32 / self.sr;
        let tau = (glide_ms.max(0.5)) * 0.001;
        let a = 1.0 - (-dt / tau).exp();
        for i in 0..3 {
            self.smoothed[i] += (self.raw[i] - self.smoothed[i]) * a;
        }
    }
}

/// Largest local maximum in `env[lo..=hi]`, returned as a fractional bin index
/// via parabolic interpolation. `None` if the range is empty or flat-zero.
fn pick_peak(env: &[f32], lo: usize, hi: usize) -> Option<f32> {
    if lo >= hi || hi >= env.len() {
        return None;
    }
    let mut best = lo;
    let mut best_v = 0.0f32;
    for k in lo..=hi {
        let v = env[k];
        if v > best_v {
            best_v = v;
            best = k;
        }
    }
    if best_v <= 0.0 {
        return None;
    }
    // Parabolic interpolation on the three points around the peak.
    if best == 0 || best + 1 >= env.len() {
        return Some(best as f32);
    }
    let (a, b, c) = (env[best - 1], env[best], env[best + 1]);
    let denom = a - 2.0 * b + c;
    let delta = if denom.abs() > 1e-20 {
        (0.5 * (a - c) / denom).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    Some(best as f32 + delta)
}
