//! Spectral de-reverberation: suppresses the late reverberant tail.
//!
//! Ported from FilmCraft `filmcraft-audio-dsp` `effects/essential.rs` (MIT/Apache-2.0, (c) 2026
//! ArtCraft Team and the FilmCraft contributors, github.com/storytold/filmcraft).
//!
//! Statistical late-reverb model (Polack): the late-reverb PSD at frame t is predicted from the
//! reverberant PSD `T_d` earlier, λ_r(t) = e^{−2Δ·T_d}·λ_x(t − T_d), Δ = 3·ln10 / RT60, and removed
//! by spectral subtraction with over-subtraction and a gain floor (Lebart et al. 2001;
//! Habets 2007). `amount` 0 = identity apart from the STFT delay, 100 % ≈ −18 dB floor.

use crate::stft::{BinGains, SpectralRunner};

/// Late-reverberation delay T_d (s): reflections later than this count as reverb.
const TD_S: f64 = 0.05;
/// Time constant (s) of the PSD smoothing.
const PSD_TAU_S: f64 = 0.02;

/// What every channel reads each frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReverbParams {
    /// e^{−2Δ·T_d} (power decay over the delay).
    decay: f32,
    beta: f32,
    floor: f32,
    alpha: f32,
}

#[derive(Clone, Debug)]
struct ReverbState {
    psd: Vec<f32>,
    /// Ring of past smoothed PSDs, `delay` frames deep.
    hist: Vec<f32>,
    delay: usize,
    pos: usize,
    gains: Vec<f32>,
    frames: usize,
}

impl ReverbState {
    fn new(bins: usize, delay: usize) -> Self {
        ReverbState { psd: vec![0.0; bins], hist: vec![0.0; bins * delay], delay, pos: 0, gains: vec![1.0; bins], frames: 0 }
    }
    fn reset(&mut self) {
        self.psd.iter_mut().for_each(|v| *v = 0.0);
        self.hist.iter_mut().for_each(|v| *v = 0.0);
        self.gains.iter_mut().for_each(|v| *v = 1.0);
        self.pos = 0;
        self.frames = 0;
    }
}

impl BinGains for ReverbState {
    type Params = ReverbParams;

    fn update(&mut self, power: &[f32], p: &ReverbParams) {
        let warm = self.frames >= self.delay;
        let bins = self.psd.len();
        let base = self.pos * bins;
        for (k, &pw) in power.iter().enumerate() {
            let psd = p.alpha * self.psd[k] + (1.0 - p.alpha) * pw;
            self.psd[k] = if psd < 1e-30 { 0.0 } else { psd };
            // The ring slot about to be overwritten holds the PSD `delay` frames ago.
            let late = if warm { p.decay * self.hist[base + k] } else { 0.0 };
            self.hist[base + k] = self.psd[k];
            let target = if self.psd[k] > 0.0 { (1.0 - p.beta * late / self.psd[k]).max(p.floor) } else { 1.0 };
            let g = &mut self.gains[k];
            // Fast recovery on onsets, slower suppression (less musical noise).
            let c = if target > *g { 0.8 } else { 0.5 };
            *g += (target - *g) * c;
        }
        self.pos = (self.pos + 1) % self.delay;
        self.frames = self.frames.saturating_add(1);
    }

    fn gains(&self) -> &[f32] {
        &self.gains
    }
}

/// Late-reverb suppressor, planar per channel, latency = FFT size.
pub struct Dereverb {
    sr: f32,
    runner: SpectralRunner,
    state: Vec<ReverbState>,
    params: ReverbParams,
}

impl Dereverb {
    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let runner = SpectralRunner::new(sample_rate, channels);
        let hop = runner.hop();
        let delay = ((TD_S * sample_rate as f64 / hop as f64).round() as usize).max(1);
        let state = vec![ReverbState::new(runner.bins(), delay); channels.max(1)];
        let alpha = (-(hop as f64) / (sample_rate as f64 * PSD_TAU_S)).exp() as f32;
        let mut d = Dereverb { sr: sample_rate, runner, state, params: ReverbParams { decay: 1.0, beta: 1.0, floor: 1.0, alpha } };
        d.set_params(50.0, 0.8);
        d
    }

    /// `amount_pct` 0..100, `rt60_s` the room's assumed decay, 0.1..5 s.
    pub fn set_params(&mut self, amount_pct: f32, rt60_s: f32) {
        let amount = amount_pct.clamp(0.0, 100.0) / 100.0;
        let rt60 = rt60_s.clamp(0.1, 5.0) as f64;
        let delta = 3.0 * std::f64::consts::LN_10 / rt60;
        let td = self.state[0].delay as f64 * self.runner.hop() as f64 / self.sr as f64;
        self.params.decay = (-2.0 * delta * td).exp() as f32;
        self.params.beta = 1.0 + amount;
        self.params.floor = 10f32.powf(-18.0 * amount / 20.0);
    }

    pub fn latency(&self) -> usize {
        self.runner.latency()
    }

    pub fn reset(&mut self) {
        self.runner.reset();
        self.state.iter_mut().for_each(ReverbState::reset);
    }

    pub fn process(&mut self, channels: &mut [&mut [f32]]) {
        self.runner.process(channels, &mut self.state, &self.params);
    }
}
