//! Spectral de-reverberation: suppresses the late reverberant tail.
//!
//! Ported from FilmCraft `filmcraft-audio-dsp` `effects/essential.rs` (MIT/Apache-2.0, (c) 2026
//! ArtCraft Team and the FilmCraft contributors, github.com/storytold/filmcraft).
//!
//! Statistical late-reverb model (Polack): the late-reverb PSD at frame t is predicted from the
//! reverberant PSD `T_d` earlier, λ_r(t) = e^{−2Δ·T_d}·λ_x(t − T_d), Δ = 3·ln10 / RT60, and removed
//! by spectral subtraction with over-subtraction and a gain floor (Lebart et al. 2001;
//! Habets 2007). `amount` 0 = identity apart from the STFT delay, 100 % ≈ −18 dB floor.

use crate::denoise::{apply_gains, smooth_gains};
use crate::stft::{fft_size, Spectral, StftChannel};

/// Late-reverberation delay T_d (s): reflections later than this count as reverb.
const TD_S: f64 = 0.05;
/// Time constant (s) of the PSD smoothing.
const PSD_TAU_S: f64 = 0.02;

#[derive(Clone, Debug)]
struct ReverbState {
    psd: Vec<f32>,
    /// Ring of past smoothed PSDs, `delay` frames deep.
    hist: Vec<f32>,
    pos: usize,
    gains: Vec<f32>,
    frames: usize,
}

impl ReverbState {
    fn new(bins: usize, delay: usize) -> Self {
        ReverbState { psd: vec![0.0; bins], hist: vec![0.0; bins * delay], pos: 0, gains: vec![1.0; bins], frames: 0 }
    }
    fn reset(&mut self) {
        self.psd.iter_mut().for_each(|v| *v = 0.0);
        self.hist.iter_mut().for_each(|v| *v = 0.0);
        self.gains.iter_mut().for_each(|v| *v = 1.0);
        self.pos = 0;
        self.frames = 0;
    }
}

/// Late-reverb suppressor, planar per channel, latency = FFT size.
pub struct Dereverb {
    sr: f32,
    n: usize,
    hop: usize,
    delay: usize,
    spec: Spectral,
    stft: Vec<StftChannel>,
    state: Vec<ReverbState>,
    smoothed_gains: Vec<f32>,
    /// e^{−2Δ·T_d} (power decay over the delay).
    decay: f32,
    beta: f32,
    floor: f32,
    alpha: f32,
}

impl Dereverb {
    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let n = fft_size(sample_rate);
        let hop = n / 4;
        let bins = n / 2 + 1;
        let delay = ((TD_S * sample_rate as f64 / hop as f64).round() as usize).max(1);
        let ch = channels.max(1);
        let mut d = Dereverb {
            sr: sample_rate,
            n,
            hop,
            delay,
            spec: Spectral::new(n, true),
            stft: vec![StftChannel::new(n); ch],
            state: vec![ReverbState::new(bins, delay); ch],
            smoothed_gains: vec![1.0; bins],
            decay: 1.0,
            beta: 1.0,
            floor: 1.0,
            alpha: (-(hop as f64) / (sample_rate as f64 * PSD_TAU_S)).exp() as f32,
        };
        d.set_params(50.0, 0.8);
        d
    }

    /// `amount_pct` 0..100, `rt60_s` the room's assumed decay, 0.1..5 s.
    pub fn set_params(&mut self, amount_pct: f32, rt60_s: f32) {
        let amount = amount_pct.clamp(0.0, 100.0) / 100.0;
        let rt60 = rt60_s.clamp(0.1, 5.0) as f64;
        let delta = 3.0 * std::f64::consts::LN_10 / rt60;
        let td = self.delay as f64 * self.hop as f64 / self.sr as f64;
        self.decay = (-2.0 * delta * td).exp() as f32;
        self.beta = 1.0 + amount;
        self.floor = 10f32.powf(-18.0 * amount / 20.0);
    }

    pub fn latency(&self) -> usize {
        self.n
    }

    pub fn reset(&mut self) {
        self.stft.iter_mut().for_each(StftChannel::reset);
        self.state.iter_mut().for_each(ReverbState::reset);
    }

    pub fn process(&mut self, channels: &mut [&mut [f32]]) {
        let nch = channels.len().min(self.stft.len());
        let (n, hop, delay) = (self.n, self.hop, self.delay);
        let (decay, beta, floor, alpha) = (self.decay, self.beta, self.floor, self.alpha);
        let scale = 1.0 / (n as f32 * 2.0);
        let bins = n / 2 + 1;
        for (ch, buf) in channels.iter_mut().enumerate().take(nch) {
            let st = &mut self.state[ch];
            let spec = &mut self.spec;
            let sg = &mut self.smoothed_gains;
            let mut frame = |input: &[f32], out: &mut [f32]| {
                spec.analyse(input);
                let warm = st.frames >= delay;
                let base = st.pos * bins;
                for k in 0..bins {
                    let p = spec.re[k] * spec.re[k] + spec.im[k] * spec.im[k];
                    let psd = alpha * st.psd[k] + (1.0 - alpha) * p;
                    st.psd[k] = if psd < 1e-30 { 0.0 } else { psd };
                    // The ring slot about to be overwritten holds the PSD `delay` frames ago.
                    let late = if warm { decay * st.hist[base + k] } else { 0.0 };
                    st.hist[base + k] = st.psd[k];
                    let target = if st.psd[k] > 0.0 { (1.0 - beta * late / st.psd[k]).max(floor) } else { 1.0 };
                    let g = &mut st.gains[k];
                    // Fast recovery on onsets, slower suppression (less musical noise).
                    let c = if target > *g { 0.8 } else { 0.5 };
                    *g += (target - *g) * c;
                }
                st.pos = (st.pos + 1) % delay;
                st.frames = st.frames.saturating_add(1);
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
