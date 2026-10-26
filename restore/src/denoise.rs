//! Broadband noise reduction by spectral gating.
//!
//! The gating and the minimum-statistics noise tracker are ported from FilmCraft
//! `filmcraft-audio-dsp` `effects/restoration.rs` (MIT/Apache-2.0, (c) 2026 ArtCraft Team and
//! the FilmCraft contributors, github.com/storytold/filmcraft). Added here: a **learned profile**.
//!
//! Two ways to know what the noise is:
//! * **adaptive** (default): every bin's floor is the bias-corrected minimum of its smoothed
//!   power over the last ~1.5 s (Martin 2001). No learn step, follows changing noise, but a
//!   tone that never stops (a held note, a fridge hum you want to keep) reads as noise.
//! * **learned**: `learn(true)` while only noise plays, `learn(false)` to freeze the mean power
//!   per bin as the floor. Stationary program survives, and the floor does not drift.
//!
//! Bins under `floor × threshold` are pulled down by `reduction`, with per-bin attack/release
//! and 3-tap frequency smoothing against musical noise.

use crate::stft::{fft_size, Spectral, StftChannel};

/// Minimum-statistics sub-windows.
const MS_SUBWINDOWS: usize = 8;
/// Length of the minimum-statistics search window (seconds).
const MS_WINDOW_S: f32 = 1.5;
/// Bias compensation of the minimum of smoothed periodograms (≈ +4 dB).
const MS_BIAS: f32 = 2.5;
const PSD_SMOOTH: f32 = 0.8;
/// STFT frames (hop = n/4) before the analysis window is full of real input.
const WARMUP_FRAMES: usize = 4;

#[derive(Clone, Debug)]
struct NoiseState {
    psd: Vec<f32>,
    cur_min: Vec<f32>,
    ring: Vec<f32>,
    ring_pos: usize,
    frames_in_sub: usize,
    gains: Vec<f32>,
    started: bool,
    learn_sum: Vec<f64>,
    learn_frames: usize,
    /// Frames since reset; the first `WARMUP_FRAMES` still hold the zero-filled start.
    frames: usize,
    profile: Option<Vec<f32>>,
}

impl NoiseState {
    fn new(bins: usize) -> Self {
        NoiseState {
            psd: vec![0.0; bins],
            cur_min: vec![f32::INFINITY; bins],
            ring: vec![f32::INFINITY; bins * MS_SUBWINDOWS],
            ring_pos: 0,
            frames_in_sub: 0,
            gains: vec![1.0; bins],
            started: false,
            learn_sum: vec![0.0; bins],
            learn_frames: 0,
            frames: 0,
            profile: None,
        }
    }

    /// Clears the running state; a learned profile is a setting and survives.
    fn reset(&mut self) {
        self.psd.iter_mut().for_each(|v| *v = 0.0);
        self.cur_min.iter_mut().for_each(|v| *v = f32::INFINITY);
        self.ring.iter_mut().for_each(|v| *v = f32::INFINITY);
        self.gains.iter_mut().for_each(|v| *v = 1.0);
        self.ring_pos = 0;
        self.frames_in_sub = 0;
        self.started = false;
        self.frames = 0;
    }

    fn freeze_profile(&mut self) {
        if self.learn_frames == 0 {
            return;
        }
        let n = self.learn_frames as f64;
        self.profile = Some(self.learn_sum.iter().map(|s| (s / n) as f32).collect());
        self.learn_sum.iter_mut().for_each(|v| *v = 0.0);
        self.learn_frames = 0;
    }

    /// Track the minimum-statistics floor (always, so switching back to adaptive is warm), and
    /// accumulate the learned profile while learning.
    fn update(&mut self, power: &[f32], sub_frames: usize, learning: bool) {
        let learning = learning && self.frames >= WARMUP_FRAMES;
        self.frames = self.frames.saturating_add(1);
        let bins = self.psd.len();
        for (k, &p) in power.iter().enumerate() {
            self.psd[k] = if self.started { PSD_SMOOTH * self.psd[k] + (1.0 - PSD_SMOOTH) * p } else { p };
            self.cur_min[k] = self.cur_min[k].min(self.psd[k]);
            if learning {
                self.learn_sum[k] += p as f64;
            }
        }
        if learning {
            self.learn_frames += 1;
        }
        self.started = true;
        self.frames_in_sub += 1;
        if self.frames_in_sub == sub_frames {
            for k in 0..bins {
                self.ring[self.ring_pos * bins + k] = self.cur_min[k];
                self.cur_min[k] = f32::INFINITY;
            }
            self.ring_pos = (self.ring_pos + 1) % MS_SUBWINDOWS;
            self.frames_in_sub = 0;
        }
    }

    /// Noise power estimate for bin `k`: the learned profile if there is one, else the
    /// bias-corrected running minimum.
    fn noise(&self, k: usize) -> f32 {
        if let Some(p) = &self.profile {
            return p[k];
        }
        let bins = self.psd.len();
        let mut m = self.cur_min[k];
        for u in 0..MS_SUBWINDOWS {
            m = m.min(self.ring[u * bins + k]);
        }
        m * MS_BIAS
    }
}

/// Spectral-gating noise reducer, planar `&mut [f32]` per channel, latency = FFT size.
pub struct Denoiser {
    n: usize,
    hop: usize,
    sub_frames: usize,
    spec: Spectral,
    stft: Vec<StftChannel>,
    noise: Vec<NoiseState>,
    power: Vec<f32>,
    smoothed_gains: Vec<f32>,
    floor: f32,
    thr: f32,
    release: f32,
    learning: bool,
}

impl Denoiser {
    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let n = fft_size(sample_rate);
        let hop = n / 4;
        let bins = n / 2 + 1;
        let sub_frames = ((MS_WINDOW_S * sample_rate / hop as f32 / MS_SUBWINDOWS as f32).round() as usize).max(1);
        let ch = channels.max(1);
        let mut d = Denoiser {
            n,
            hop,
            sub_frames,
            spec: Spectral::new(n, true),
            stft: vec![StftChannel::new(n); ch],
            noise: vec![NoiseState::new(bins); ch],
            power: vec![0.0; bins],
            smoothed_gains: vec![1.0; bins],
            floor: 1.0,
            thr: 1.0,
            release: 0.5,
            learning: false,
        };
        d.set_params(12.0, 6.0, 50.0);
        d
    }

    /// `reduction_db` 0..40 (how far gated bins drop), `threshold_db` 0..20 (how far above the
    /// floor a bin must rise to pass), `smoothing_pct` 0..100 (release speed of the gains).
    pub fn set_params(&mut self, reduction_db: f32, threshold_db: f32, smoothing_pct: f32) {
        self.floor = 10f32.powf(-reduction_db.clamp(0.0, 60.0) / 20.0);
        self.thr = 10f32.powf(threshold_db.clamp(0.0, 30.0) / 10.0);
        // 0 % → fast (0.6 per frame), 100 % → slow (0.03 per frame).
        self.release = 0.6 - 0.57 * smoothing_pct.clamp(0.0, 100.0) / 100.0;
    }

    /// Start (`true`) or finish (`false`) learning the noise profile from the input.
    /// Finishing with nothing heard keeps the previous profile.
    pub fn learn(&mut self, on: bool) {
        if self.learning && !on {
            self.noise.iter_mut().for_each(NoiseState::freeze_profile);
        }
        self.learning = on;
    }

    pub fn is_learning(&self) -> bool {
        self.learning
    }

    pub fn has_profile(&self) -> bool {
        self.noise.iter().any(|s| s.profile.is_some())
    }

    /// Back to adaptive tracking.
    pub fn clear_profile(&mut self) {
        self.noise.iter_mut().for_each(|s| s.profile = None);
    }

    pub fn latency(&self) -> usize {
        self.n
    }

    pub fn reset(&mut self) {
        self.stft.iter_mut().for_each(StftChannel::reset);
        self.noise.iter_mut().for_each(NoiseState::reset);
    }

    pub fn process(&mut self, channels: &mut [&mut [f32]]) {
        let nch = channels.len().min(self.stft.len());
        let (n, hop, sub_frames, learning) = (self.n, self.hop, self.sub_frames, self.learning);
        let (floor, thr, release) = (self.floor, self.thr, self.release);
        let scale = 1.0 / (n as f32 * 2.0);
        let bins = n / 2 + 1;
        for (ch, buf) in channels.iter_mut().enumerate().take(nch) {
            let st = &mut self.noise[ch];
            let spec = &mut self.spec;
            let sg = &mut self.smoothed_gains;
            let power = &mut self.power;
            let mut frame = |input: &[f32], out: &mut [f32]| {
                spec.analyse(input);
                for ((p, re), im) in power.iter_mut().zip(&spec.re).zip(&spec.im) {
                    *p = re * re + im * im;
                }
                st.update(power, sub_frames, learning);
                for k in 0..bins {
                    let target = if st.psd[k] > st.noise(k) * thr { 1.0 } else { floor };
                    let g = &mut st.gains[k];
                    let c = if target > *g { 0.7 } else { release };
                    *g += (target - *g) * c;
                }
                smooth_gains(&st.gains, sg);
                apply_gains(spec, sg);
                spec.synthesise_add(out, scale);
            };
            for v in buf.iter_mut() {
                *v = self.stft[ch].tick(*v, hop, &mut frame);
            }
        }
    }
}

/// 3-tap frequency smoothing of per-bin gains (reduces musical noise).
pub(crate) fn smooth_gains(gains: &[f32], out: &mut [f32]) {
    let bins = gains.len();
    for k in 0..bins {
        let a = gains[k.saturating_sub(1)];
        let c = gains[(k + 1).min(bins - 1)];
        out[k] = 0.25 * a + 0.5 * gains[k] + 0.25 * c;
    }
}

pub(crate) fn apply_gains(spec: &mut Spectral, gains: &[f32]) {
    for (k, &g) in gains.iter().enumerate() {
        spec.re[k] *= g;
        spec.im[k] *= g;
    }
}
