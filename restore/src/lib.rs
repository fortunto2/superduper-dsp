//! SuperDuper Restore — audio restoration DSP, zero dependencies by default.
//!
//! One processor, several modes, so a plugin knob or a video pipeline flag picks the algorithm:
//!
//! | Mode | What it removes | Algorithm |
//! |---|---|---|
//! | [`Mode::Spectral`] | broadband noise: hiss, wind bed, room tone, fan | STFT spectral gating, adaptive (minimum statistics) or a learned profile |
//! | [`Mode::Dereverb`] | late room reverb on a voice | statistical late-reverb spectral subtraction |
//! | [`Mode::Neural`] | anything that is not a voice | RNNoise (feature `neural`, 48 kHz only); otherwise an exact delayed passthrough |
//!
//! Planar `&mut [f32]` per channel in, same slices out, any block size. Every mode reports the
//! same latency ([`Restorer::latency`]), so switching modes never shifts the timeline; offline
//! callers use [`process_offline`], which compensates it. [`Settings::mix_pct`] blends the
//! latency-aligned dry signal back in, in every mode.
//!
//! The spectral algorithms are ported from FilmCraft `filmcraft-audio-dsp` (MIT/Apache-2.0,
//! (c) 2026 ArtCraft Team and the FilmCraft contributors, github.com/storytold/filmcraft); see
//! each module's header. Neural is `nnnoiseless` (BSD-3-Clause).

mod denoise;
mod dereverb;
mod fft;
#[cfg(feature = "neural")]
mod neural;
mod stft;

pub use denoise::Denoiser;
pub use dereverb::Dereverb;
pub use fft::Fft;

/// Which restoration runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Spectral,
    Dereverb,
    /// RNNoise when built with `neural` and running at 48 kHz; otherwise the audio passes
    /// through unchanged, delayed by the common latency ([`Restorer::neural_ready`] says which).
    Neural,
}

impl Mode {
    pub const ALL: [Mode; 3] = [Mode::Spectral, Mode::Dereverb, Mode::Neural];

    /// Plugin parameter value (0, 1, 2) → mode; out of range clamps.
    pub fn from_index(i: u32) -> Mode {
        match i {
            0 => Mode::Spectral,
            1 => Mode::Dereverb,
            _ => Mode::Neural,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mode::Spectral => "Spectral",
            Mode::Dereverb => "Dereverb",
            Mode::Neural => "Neural",
        }
    }

    /// Whether this build can change the sound in this mode (Neural needs the feature).
    pub fn is_implemented(self) -> bool {
        !matches!(self, Mode::Neural) || cfg!(feature = "neural")
    }
}

/// Every knob of every mode; a mode reads only its own, `mix_pct` applies to all.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub mode: Mode,
    /// Spectral: how far gated bins drop, dB (0..40).
    pub reduction_db: f32,
    /// Spectral: how far above the noise floor a bin must rise to pass, dB (0..20).
    pub threshold_db: f32,
    /// Spectral: gain release smoothing, % (0..100).
    pub smoothing_pct: f32,
    /// Dereverb: strength, % (0..100).
    pub amount_pct: f32,
    /// Dereverb: assumed room decay, s (0.1..5).
    pub rt60_s: f32,
    /// Processed share of the output, % (0 = dry, 100 = fully processed).
    pub mix_pct: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { mode: Mode::Spectral, reduction_db: 12.0, threshold_db: 6.0, smoothing_pct: 50.0, amount_pct: 50.0, rt60_s: 0.8, mix_pct: 100.0 }
    }
}

/// Fixed per-channel delay line (latency padding, aligned dry signal).
pub(crate) struct Delay {
    lines: Vec<Vec<f32>>,
    pos: Vec<usize>,
}

impl Delay {
    pub(crate) fn new(len: usize, channels: usize) -> Self {
        Delay { lines: vec![vec![0.0; len]; channels], pos: vec![0; channels] }
    }
    pub(crate) fn reset(&mut self) {
        self.lines.iter_mut().for_each(|l| l.iter_mut().for_each(|v| *v = 0.0));
        self.pos.iter_mut().for_each(|p| *p = 0);
    }
    pub(crate) fn process(&mut self, channels: &mut [&mut [f32]]) {
        for (ch, buf) in channels.iter_mut().enumerate().take(self.lines.len()) {
            let line = &mut self.lines[ch];
            if line.is_empty() {
                continue;
            }
            let pos = &mut self.pos[ch];
            for v in buf.iter_mut() {
                *v = std::mem::replace(&mut line[*pos], *v);
                *pos = (*pos + 1) % line.len();
            }
        }
    }
}

/// Neural when it can run, a latency-matched passthrough when it cannot.
enum NeuralPath {
    #[cfg(feature = "neural")]
    Net(neural::Neural),
    Pass(Delay),
}

/// Samples per internal chunk when mixing (the dry copy lives in a fixed scratch buffer).
const MIX_CHUNK: usize = 512;

/// All modes behind one interface. Each mode keeps its own state; switching resets the one
/// being switched to, so it never resumes from stale spectra.
pub struct Restorer {
    settings: Settings,
    denoise: Denoiser,
    dereverb: Dereverb,
    neural: NeuralPath,
    dry: Delay,
    scratch: Vec<[f32; MIX_CHUNK]>,
}

impl Restorer {
    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let ch = channels.max(1);
        let denoise = Denoiser::new(sample_rate, ch);
        let latency = denoise.latency();
        #[cfg(feature = "neural")]
        let neural = if sample_rate == neural::RATE { NeuralPath::Net(neural::Neural::new(ch, latency)) } else { NeuralPath::Pass(Delay::new(latency, ch)) };
        #[cfg(not(feature = "neural"))]
        let neural = NeuralPath::Pass(Delay::new(latency, ch));
        let mut r = Restorer {
            settings: Settings::default(),
            denoise,
            dereverb: Dereverb::new(sample_rate, ch),
            neural,
            dry: Delay::new(latency, ch),
            scratch: vec![[0.0; MIX_CHUNK]; ch],
        };
        r.set(Settings::default());
        r
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Whether Neural mode runs the network (built with `neural`, 48 kHz). When false it is an
    /// exact passthrough with the common latency.
    pub fn neural_ready(&self) -> bool {
        !matches!(self.neural, NeuralPath::Pass(_))
    }

    /// Apply settings; cheap when nothing changed, call it every block.
    pub fn set(&mut self, s: Settings) {
        if s.mode != self.settings.mode {
            match s.mode {
                Mode::Spectral => self.denoise.reset(),
                Mode::Dereverb => self.dereverb.reset(),
                Mode::Neural => self.reset_neural(),
            }
        }
        self.denoise.set_params(s.reduction_db, s.threshold_db, s.smoothing_pct);
        self.dereverb.set_params(s.amount_pct, s.rt60_s);
        self.settings = s;
    }

    fn reset_neural(&mut self) {
        match &mut self.neural {
            #[cfg(feature = "neural")]
            NeuralPath::Net(n) => n.reset(),
            NeuralPath::Pass(d) => d.reset(),
        }
    }

    /// Spectral mode: learn the noise profile while `true`, freeze it on `false`.
    pub fn learn(&mut self, on: bool) {
        self.denoise.learn(on);
    }

    pub fn denoiser(&self) -> &Denoiser {
        &self.denoise
    }

    pub fn denoiser_mut(&mut self) -> &mut Denoiser {
        &mut self.denoise
    }

    pub fn clear_profile(&mut self) {
        self.denoise.clear_profile();
    }

    /// Samples of delay, identical in every mode.
    pub fn latency(&self) -> usize {
        self.denoise.latency()
    }

    pub fn reset(&mut self) {
        self.denoise.reset();
        self.dereverb.reset();
        self.reset_neural();
        self.dry.reset();
    }

    pub fn process(&mut self, channels: &mut [&mut [f32]]) {
        let mix = (self.settings.mix_pct / 100.0).clamp(0.0, 1.0);
        let nch = channels.len().min(self.scratch.len());
        let len = channels.iter().take(nch).map(|c| c.len()).min().unwrap_or(0);
        let mut at = 0;
        // The dry line runs at every mix, so moving the Mix knob never exposes stale audio.
        while at < len {
            let n = MIX_CHUNK.min(len - at);
            for (dry, src) in self.scratch.iter_mut().zip(channels.iter()).take(nch) {
                dry[..n].copy_from_slice(&src[at..at + n]);
            }
            self.mix_chunk(channels, nch, at, n, mix);
            at += n;
        }
    }

    /// Process `at..at+n` in place (wet), delay the dry copy by the common latency, blend.
    /// Mono and stereo split slices without allocating; more channels collect a Vec (offline).
    fn mix_chunk(&mut self, channels: &mut [&mut [f32]], nch: usize, at: usize, n: usize, mix: f32) {
        match nch {
            1 => {
                self.run_mode(&mut [&mut channels[0][at..at + n]]);
                self.dry.process(&mut [&mut self.scratch[0][..n]]);
            }
            2 => {
                let (a, b) = channels.split_at_mut(1);
                self.run_mode(&mut [&mut a[0][at..at + n], &mut b[0][at..at + n]]);
                let (a, b) = self.scratch.split_at_mut(1);
                self.dry.process(&mut [&mut a[0][..n], &mut b[0][..n]]);
            }
            _ => {
                let mut wet: Vec<&mut [f32]> = channels.iter_mut().take(nch).map(|c| &mut c[at..at + n]).collect();
                self.run_mode(&mut wet);
                let mut dry: Vec<&mut [f32]> = self.scratch.iter_mut().take(nch).map(|c| &mut c[..n]).collect();
                self.dry.process(&mut dry);
            }
        }
        if mix >= 1.0 {
            return;
        }
        for (wet, dry) in channels.iter_mut().zip(&self.scratch).take(nch) {
            for (w, &d) in wet[at..at + n].iter_mut().zip(&dry[..n]) {
                *w = d + mix * (*w - d);
            }
        }
    }

    fn run_mode(&mut self, channels: &mut [&mut [f32]]) {
        match self.settings.mode {
            Mode::Spectral => self.denoise.process(channels),
            Mode::Dereverb => self.dereverb.process(channels),
            Mode::Neural => match &mut self.neural {
                #[cfg(feature = "neural")]
                NeuralPath::Net(n) => n.process(channels),
                NeuralPath::Pass(d) => d.process(channels),
            },
        }
    }
}

/// Restore whole planar buffers in place, latency compensated: output sample i lines up with
/// input sample i. `noise_profile` is an optional `(start, len)` sample range holding only noise
/// (for example the room before anyone speaks); Spectral mode learns from it first.
pub fn process_offline(channels: &mut [Vec<f32>], sample_rate: f32, settings: Settings, noise_profile: Option<(usize, usize)>) {
    let nch = channels.len();
    if nch == 0 {
        return;
    }
    let mut r = Restorer::new(sample_rate, nch);
    r.set(settings);
    if let (Mode::Spectral, Some((start, len))) = (settings.mode, noise_profile) {
        let mut probe: Vec<Vec<f32>> = channels.iter().map(|c| c.iter().skip(start).take(len).copied().collect()).collect();
        r.learn(true);
        let mut views: Vec<&mut [f32]> = probe.iter_mut().map(|c| c.as_mut_slice()).collect();
        r.process(&mut views);
        r.learn(false);
        r.reset();
    }
    let lat = r.latency();
    let len = channels.iter().map(Vec::len).max().unwrap_or(0);
    let mut padded: Vec<Vec<f32>> = channels
        .iter()
        .map(|c| {
            let mut v = c.clone();
            v.resize(len + lat, 0.0);
            v
        })
        .collect();
    {
        let mut views: Vec<&mut [f32]> = padded.iter_mut().map(|c| c.as_mut_slice()).collect();
        r.process(&mut views);
    }
    for (c, p) in channels.iter_mut().zip(padded) {
        let n = c.len();
        c.copy_from_slice(&p[lat..lat + n]);
    }
}
