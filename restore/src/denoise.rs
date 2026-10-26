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

use crate::stft::{BinGains, SpectralRunner};

/// Minimum-statistics sub-windows.
const MS_SUBWINDOWS: usize = 8;
/// Length of the minimum-statistics search window (seconds).
const MS_WINDOW_S: f32 = 1.5;
/// Bias compensation of the minimum of smoothed periodograms (≈ +4 dB).
const MS_BIAS: f32 = 2.5;
const PSD_SMOOTH: f32 = 0.8;
/// STFT frames (hop = n/4) before the analysis window is full of real input.
const WARMUP_FRAMES: usize = 4;

/// What every channel reads each frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GateParams {
    floor: f32,
    thr: f32,
    release: f32,
    sub_frames: usize,
    learning: bool,
}

#[derive(Clone, Debug)]
struct NoiseState {
    psd: Vec<f32>,
    cur_min: Vec<f32>,
    ring: Vec<f32>,
    /// Per-bin minimum over `ring`, refreshed when the ring rotates (once per sub-window), so
    /// the per-frame floor is one `min` instead of a strided walk over every sub-window.
    ring_min: Vec<f32>,
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
            ring_min: vec![f32::INFINITY; bins],
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
        self.ring_min.iter_mut().for_each(|v| *v = f32::INFINITY);
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
    fn track(&mut self, power: &[f32], sub_frames: usize, learning: bool) {
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
            let slot = &mut self.ring[self.ring_pos * bins..(self.ring_pos + 1) * bins];
            slot.copy_from_slice(&self.cur_min);
            self.cur_min.iter_mut().for_each(|v| *v = f32::INFINITY);
            self.ring_pos = (self.ring_pos + 1) % MS_SUBWINDOWS;
            self.frames_in_sub = 0;
            self.ring_min.copy_from_slice(&self.ring[..bins]);
            for sub in self.ring[bins..].chunks_exact(bins) {
                for (m, &v) in self.ring_min.iter_mut().zip(sub) {
                    *m = m.min(v);
                }
            }
        }
    }

    /// Noise power estimate for bin `k`: the learned profile if there is one, else the
    /// bias-corrected running minimum.
    fn noise(&self, k: usize) -> f32 {
        match &self.profile {
            Some(p) => p[k],
            None => self.cur_min[k].min(self.ring_min[k]) * MS_BIAS,
        }
    }
}

impl BinGains for NoiseState {
    type Params = GateParams;

    fn update(&mut self, power: &[f32], p: &GateParams) {
        self.track(power, p.sub_frames, p.learning);
        for k in 0..self.gains.len() {
            let target = if self.psd[k] > self.noise(k) * p.thr { 1.0 } else { p.floor };
            let g = &mut self.gains[k];
            let c = if target > *g { 0.7 } else { p.release };
            *g += (target - *g) * c;
        }
    }

    fn gains(&self) -> &[f32] {
        &self.gains
    }
}

/// Spectral-gating noise reducer, planar `&mut [f32]` per channel, latency = FFT size.
pub struct Denoiser {
    runner: SpectralRunner,
    noise: Vec<NoiseState>,
    params: GateParams,
}

impl Denoiser {
    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let runner = SpectralRunner::new(sample_rate, channels);
        let sub_frames = ((MS_WINDOW_S * sample_rate / runner.hop() as f32 / MS_SUBWINDOWS as f32).round() as usize).max(1);
        let noise = vec![NoiseState::new(runner.bins()); channels.max(1)];
        let mut d = Denoiser { runner, noise, params: GateParams { floor: 1.0, thr: 1.0, release: 0.5, sub_frames, learning: false } };
        d.set_params(12.0, 6.0, 50.0);
        d
    }

    /// `reduction_db` 0..40 (how far gated bins drop), `threshold_db` 0..20 (how far above the
    /// floor a bin must rise to pass), `smoothing_pct` 0..100 (release speed of the gains).
    pub fn set_params(&mut self, reduction_db: f32, threshold_db: f32, smoothing_pct: f32) {
        self.params.floor = 10f32.powf(-reduction_db.clamp(0.0, 60.0) / 20.0);
        self.params.thr = 10f32.powf(threshold_db.clamp(0.0, 30.0) / 10.0);
        // 0 % → fast (0.6 per frame), 100 % → slow (0.03 per frame).
        self.params.release = 0.6 - 0.57 * smoothing_pct.clamp(0.0, 100.0) / 100.0;
    }

    /// Start (`true`) or finish (`false`) learning the noise profile from the input.
    /// Finishing with nothing heard keeps the previous profile.
    pub fn learn(&mut self, on: bool) {
        if self.params.learning && !on {
            self.noise.iter_mut().for_each(NoiseState::freeze_profile);
        }
        self.params.learning = on;
    }

    pub fn is_learning(&self) -> bool {
        self.params.learning
    }

    pub fn has_profile(&self) -> bool {
        self.noise.iter().any(|s| s.profile.is_some())
    }

    /// The learned profile, one power spectrum (n/2+1 bins) per channel, to persist it.
    pub fn profile(&self) -> Option<Vec<Vec<f32>>> {
        self.noise.iter().map(|s| s.profile.clone()).collect()
    }

    /// Install a profile saved from [`Denoiser::profile`]. Refused (false, nothing changed) when
    /// the channel count or bin count differs — a profile from another sample rate is meaningless.
    pub fn set_profile(&mut self, profile: Vec<Vec<f32>>) -> bool {
        let bins = self.runner.bins();
        if profile.len() != self.noise.len() || profile.iter().any(|p| p.len() != bins || p.iter().any(|v| !v.is_finite())) {
            return false;
        }
        for (st, p) in self.noise.iter_mut().zip(profile) {
            st.profile = Some(p);
        }
        true
    }

    pub fn latency(&self) -> usize {
        self.runner.latency()
    }

    pub fn reset(&mut self) {
        self.runner.reset();
        self.noise.iter_mut().for_each(NoiseState::reset);
    }

    pub fn process(&mut self, channels: &mut [&mut [f32]]) {
        self.runner.process(channels, &mut self.noise, &self.params);
    }
}
