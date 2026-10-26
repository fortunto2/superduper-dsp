//! SuperDuper Restore — audio restoration DSP with no dependencies.
//!
//! One processor, several modes, so a plugin knob or a video pipeline flag picks the algorithm:
//!
//! | Mode | What it removes | Algorithm |
//! |---|---|---|
//! | [`Mode::Spectral`] | broadband noise: hiss, wind bed, room tone, fan | STFT spectral gating, adaptive (minimum statistics) or a learned profile |
//! | [`Mode::Dereverb`] | late room reverb on a voice | statistical late-reverb spectral subtraction |
//! | [`Mode::Neural`] | voice-vs-anything (planned) | **not yet**: passes audio through, delayed by the same latency |
//!
//! Planar `&mut [f32]` per channel in, same slices out, any block size. Every mode reports the
//! same latency ([`Restorer::latency`]), so switching modes never shifts the timeline; offline
//! callers use [`process_offline`], which compensates it.
//!
//! The spectral algorithms are ported from FilmCraft `filmcraft-audio-dsp` (MIT/Apache-2.0,
//! (c) 2026 ArtCraft Team and the FilmCraft contributors, github.com/storytold/filmcraft); see
//! each module's header.

mod denoise;
mod dereverb;
mod fft;
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
    /// AI-TODO: RNNoise via the `nnnoiseless` crate (480-sample frames at 48 kHz). Until then the
    /// audio passes through unchanged, delayed by the common latency.
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
            Mode::Neural => "Neural (soon)",
        }
    }

    /// Whether the mode actually changes the sound yet.
    pub fn is_implemented(self) -> bool {
        !matches!(self, Mode::Neural)
    }
}

/// Every knob of every mode; a mode reads only its own.
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
}

impl Default for Settings {
    fn default() -> Self {
        Settings { mode: Mode::Spectral, reduction_db: 12.0, threshold_db: 6.0, smoothing_pct: 50.0, amount_pct: 50.0, rt60_s: 0.8 }
    }
}

/// Fixed delay line, so the not-yet-implemented mode keeps the common latency.
struct Delay {
    lines: Vec<Vec<f32>>,
    pos: Vec<usize>,
}

impl Delay {
    fn new(len: usize, channels: usize) -> Self {
        Delay { lines: vec![vec![0.0; len.max(1)]; channels], pos: vec![0; channels] }
    }
    fn reset(&mut self) {
        self.lines.iter_mut().for_each(|l| l.iter_mut().for_each(|v| *v = 0.0));
        self.pos.iter_mut().for_each(|p| *p = 0);
    }
    fn process(&mut self, channels: &mut [&mut [f32]]) {
        for (ch, buf) in channels.iter_mut().enumerate().take(self.lines.len()) {
            let line = &mut self.lines[ch];
            let pos = &mut self.pos[ch];
            for v in buf.iter_mut() {
                *v = std::mem::replace(&mut line[*pos], *v);
                *pos = (*pos + 1) % line.len();
            }
        }
    }
}

/// All modes behind one interface. Each mode keeps its own state; switching resets the one
/// being switched to, so it never resumes from stale spectra.
pub struct Restorer {
    settings: Settings,
    denoise: Denoiser,
    dereverb: Dereverb,
    neural: Delay,
}

impl Restorer {
    pub fn new(sample_rate: f32, channels: usize) -> Self {
        let ch = channels.max(1);
        let denoise = Denoiser::new(sample_rate, ch);
        let latency = denoise.latency();
        let mut r = Restorer { settings: Settings::default(), denoise, dereverb: Dereverb::new(sample_rate, ch), neural: Delay::new(latency, ch) };
        r.set(Settings::default());
        r
    }

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Apply settings; cheap when nothing changed, call it every block.
    pub fn set(&mut self, s: Settings) {
        if s.mode != self.settings.mode {
            match s.mode {
                Mode::Spectral => self.denoise.reset(),
                Mode::Dereverb => self.dereverb.reset(),
                Mode::Neural => self.neural.reset(),
            }
        }
        self.denoise.set_params(s.reduction_db, s.threshold_db, s.smoothing_pct);
        self.dereverb.set_params(s.amount_pct, s.rt60_s);
        self.settings = s;
    }

    /// Spectral mode: learn the noise profile while `true`, freeze it on `false`.
    pub fn learn(&mut self, on: bool) {
        self.denoise.learn(on);
    }

    pub fn denoiser(&self) -> &Denoiser {
        &self.denoise
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
        self.neural.reset();
    }

    pub fn process(&mut self, channels: &mut [&mut [f32]]) {
        match self.settings.mode {
            Mode::Spectral => self.denoise.process(channels),
            Mode::Dereverb => self.dereverb.process(channels),
            Mode::Neural => self.neural.process(channels),
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
