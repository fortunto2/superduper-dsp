//! The spectral-mask runner Spectral and Dereverb share: per channel a streaming STFT
//! (`superduper-dsp-core`, √Hann analysis and synthesis, 75 % overlap, latency = FFT size),
//! per-bin power in, per-bin gains out. The two modes differ only in how a channel's state turns
//! power into gains ([`BinGains`]); the frame plumbing and the gain smoothing live here once.

use superduper_dsp_core::spectral::{StftProcessor, Window};

/// FFT size for spectral effects at a sample rate (~43 ms at 48 kHz).
pub(crate) fn fft_size(sample_rate: f32) -> usize {
    if sample_rate <= 50_000.0 {
        2048
    } else if sample_rate <= 100_000.0 {
        4096
    } else {
        8192
    }
}

/// One channel's mask logic: update from this frame's power spectrum, expose the raw gains.
pub(crate) trait BinGains {
    type Params;
    fn update(&mut self, power: &[f32], p: &Self::Params);
    fn gains(&self) -> &[f32];
}

pub(crate) struct SpectralRunner {
    n: usize,
    stft: Vec<StftProcessor>,
    power: Vec<f32>,
    smoothed: Vec<f32>,
}

impl SpectralRunner {
    pub(crate) fn new(sample_rate: f32, channels: usize) -> Self {
        let n = fft_size(sample_rate);
        let bins = n / 2 + 1;
        SpectralRunner {
            n,
            stft: (0..channels.max(1)).map(|_| StftProcessor::with_window(sample_rate, n, n / 4, 1, Window::SqrtHann)).collect(),
            power: vec![0.0; bins],
            smoothed: vec![1.0; bins],
        }
    }

    pub(crate) fn hop(&self) -> usize {
        self.n / 4
    }

    pub(crate) fn bins(&self) -> usize {
        self.n / 2 + 1
    }

    /// Samples of delay: one window.
    pub(crate) fn latency(&self) -> usize {
        self.n
    }

    pub(crate) fn reset(&mut self) {
        self.stft.iter_mut().for_each(StftProcessor::reset);
    }

    /// Run every channel through its STFT; each frame `states[ch]` turns power into gains, which
    /// are 3-tap smoothed across frequency and applied to the spectrum.
    pub(crate) fn process<S: BinGains>(&mut self, channels: &mut [&mut [f32]], states: &mut [S], p: &S::Params) {
        let (power, smoothed) = (&mut self.power, &mut self.smoothed);
        for ((buf, stft), st) in channels.iter_mut().zip(&mut self.stft).zip(states.iter_mut()) {
            for v in buf.iter_mut() {
                *v = stft.process_sample(&[*v], |ana, syn| {
                    for (pw, c) in power.iter_mut().zip(ana[0]) {
                        *pw = c.re * c.re + c.im * c.im;
                    }
                    st.update(power, p);
                    smooth_gains(st.gains(), smoothed);
                    for ((o, &a), &g) in syn.iter_mut().zip(ana[0]).zip(smoothed.iter()) {
                        *o = a * g;
                    }
                });
            }
        }
    }
}

/// 3-tap frequency smoothing of per-bin gains (reduces musical noise).
fn smooth_gains(gains: &[f32], out: &mut [f32]) {
    let bins = gains.len();
    for k in 0..bins {
        let a = gains[k.saturating_sub(1)];
        let c = gains[(k + 1).min(bins - 1)];
        out[k] = 0.25 * a + 0.5 * gains[k] + 0.25 * c;
    }
}
