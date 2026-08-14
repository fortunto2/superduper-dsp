//! Cheap noise primitives for SuperDuper Wind's stochastic breath layer.
//!
//! Spectral Modeling Synthesis splits a sound into a deterministic
//! (harmonic) part and a stochastic (noise residual) part — this module is
//! the stochastic side: RT-safe pseudo-random generators, no heap, no
//! syscalls, safe to call once per sample from `process()`.

use superduper_synth_core::dsp_blocks::{OnePoleLp, Xorshift};

/// The shared RT-safe PRNG under its historical local name. Wind used to carry
/// its own copy of xorshift32 with a lossy `x as f32 / u32::MAX` conversion
/// (which throws away the low bits above 2^24); `dsp_blocks::Xorshift` is the
/// one implementation the whole codebase now draws from.
pub type Xorshift32 = Xorshift;

/// Paul Kellet's "economy" pink-noise filter — three cascaded one-pole
/// filters on white noise, the standard cheap 1/f approximation (roughly
/// -3 dB/octave down to a few Hz). CLAUDE.md calls this out explicitly as
/// the intended implementation for the dark/brown "Color" end of Wind's
/// breath noise.
#[derive(Clone, Copy, Default)]
pub struct PinkFilter {
    b0: f32,
    b1: f32,
    b2: f32,
}

impl PinkFilter {
    #[inline]
    pub fn next(&mut self, white: f32) -> f32 {
        self.b0 = 0.997_65 * self.b0 + white * 0.099_046_0;
        self.b1 = 0.963_00 * self.b1 + white * 0.296_516_4;
        self.b2 = 0.570_00 * self.b2 + white * 1.052_691_3;
        (self.b0 + self.b1 + self.b2 + white * 0.1848) * 0.115
    }
}

/// One channel's stochastic noise source: xorshift white noise blended
/// with its pink-filtered derivative by `color` (0 = pink/dark/wind-like,
/// 1 = white/airy). This feeds the formant bandpass to make the "wind"
/// layer.
#[derive(Clone, Copy)]
pub struct ColorNoise {
    rng: Xorshift32,
    pink: PinkFilter,
}

impl ColorNoise {
    pub fn new(seed: u32) -> Self {
        Self {
            rng: Xorshift32::new(seed),
            pink: PinkFilter::default(),
        }
    }

    #[inline]
    pub fn next(&mut self, color: f32) -> f32 {
        let white = self.rng.next_bipolar();
        let pink = self.pink.next(white);
        let color = color.clamp(0.0, 1.0);
        pink * (1.0 - color) + white * color
    }
}

/// Slow random-wander generator for Jitter (pitch) / Shimmer (amplitude).
/// Pink noise smoothed by an extra one-pole lowpass so it reads as an
/// organic "wobble" rather than audio-rate roughness — natural breath
/// vibrato/tremolo territory rather than FM noise.
#[derive(Clone, Copy)]
pub struct WobbleGen {
    rng: Xorshift32,
    pink: PinkFilter,
    lp: OnePoleLp,
}

impl WobbleGen {
    pub fn new(seed: u32) -> Self {
        Self {
            rng: Xorshift32::new(seed),
            pink: PinkFilter::default(),
            lp: OnePoleLp::default(),
        }
    }

    /// Returns a smoothly-wandering value, roughly in `[-1, 1]`.
    /// `rate_hz` sets how fast it wanders — a few Hz sounds like natural
    /// breath instability, tens of Hz starts to sound like tremolo.
    #[inline]
    pub fn next(&mut self, sr: f32, rate_hz: f32) -> f32 {
        let white = self.rng.next_bipolar();
        let pink = self.pink.next(white);
        // Makeup gain — the lowpass eats most of the pink signal's energy,
        // this keeps the wobble at a musically useful depth before the
        // caller scales it by the Jitter/Shimmer param.
        (self.lp.process(pink, sr, rate_hz) * 3.2).clamp(-1.5, 1.5)
    }
}
