//! Drum voice DSP — six analog-synthesis instruments. Each voice is
//! a tiny state machine triggered by `trigger(velocity)` and run per
//! sample by `process(sr)` until the envelope decays to silence.
//!
//! The voices are designed to *play nice with each other* — same
//! Tune / Decay / Level / Pan API across all six so the GUI can
//! render them as identical channel strips. The synthesis under each
//! is what differs.

use std::f32::consts::TAU;

/// Parameters every voice accepts.
#[derive(Copy, Clone)]
pub struct DrumParams {
    /// Pitch offset in semitones from the voice's native centre.
    pub tune_st: f32,
    /// Decay time in seconds (envelope tau).
    pub decay_s: f32,
    /// Linear level multiplier 0..1.
    pub level: f32,
    /// -1 = full L, +1 = full R, 0 = centre.
    pub pan: f32,
}

impl Default for DrumParams {
    fn default() -> Self {
        Self { tune_st: 0.0, decay_s: 0.3, level: 0.8, pan: 0.0 }
    }
}

/// One-shot exponential envelope. Reset to 1.0 on trigger, decays to
/// zero with the configured tau. RT-safe — no heap.
#[derive(Copy, Clone, Default)]
pub struct OneShotEnv {
    pub level: f32,
}

impl OneShotEnv {
    #[inline]
    pub fn trigger(&mut self, amp: f32) { self.level = amp; }
    /// Apply exponential decay one sample at a time. Returns the
    /// current level after the step.
    #[inline]
    pub fn step(&mut self, sr: f32, decay_s: f32) -> f32 {
        let tau = (decay_s.max(0.005) * sr).max(1.0);
        let coef = (-1.0 / tau).exp();
        self.level *= coef;
        if self.level < 1e-5 { self.level = 0.0; }
        self.level
    }
    #[inline]
    pub fn is_idle(&self) -> bool { self.level <= 1e-5 }
}

/// White-noise generator using a fast xorshift32 PRNG.
#[derive(Copy, Clone)]
pub struct Noise { state: u32 }
impl Default for Noise { fn default() -> Self { Self { state: 0xDEADBEEF } } }
impl Noise {
    #[inline]
    pub fn next(&mut self) -> f32 {
        let mut s = self.state;
        s ^= s << 13; s ^= s >> 17; s ^= s << 5;
        self.state = s;
        (s as f32) / (u32::MAX as f32) * 2.0 - 1.0
    }
}

/// One-pole bandpass — biquad-equivalent in a smaller package. Used
/// for cowbell / clap / hat resonance tuning.
#[derive(Copy, Clone, Default)]
pub struct OnePoleBp { z1: f32, z2: f32, b0: f32, b1: f32, b2: f32, a1: f32, a2: f32 }
impl OnePoleBp {
    pub fn set(&mut self, sr: f32, freq: f32, q: f32) {
        let w0 = TAU * freq.max(20.0) / sr;
        let alpha = w0.sin() / (2.0 * q.max(0.1));
        let cos_w0 = w0.cos();
        let b0 = alpha;
        let b1 = 0.0;
        let b2 = -alpha;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        self.b0 = b0 / a0; self.b1 = b1 / a0; self.b2 = b2 / a0;
        self.a1 = a1 / a0; self.a2 = a2 / a0;
    }
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

// ---------------------------------------------------------------------------
// KICK — triangle / sine pitched osc with a fast pitch-drop envelope
// + click transient. The classic 808 "boom" recipe.
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Default)]
pub struct Kick {
    pitch_env: OneShotEnv,
    amp_env: OneShotEnv,
    click_env: OneShotEnv,
    phase: f32,
    noise: Noise,
}

impl Kick {
    pub fn trigger(&mut self, velocity: f32) {
        self.amp_env.trigger(velocity);
        self.pitch_env.trigger(1.0);
        self.click_env.trigger(velocity * 0.4);
        self.phase = 0.0;
    }
    pub fn is_idle(&self) -> bool { self.amp_env.is_idle() && self.click_env.is_idle() }
    pub fn process(&mut self, sr: f32, p: DrumParams) -> f32 {
        // Pitch drops from ~3× base down to base over ~50 ms.
        let pitch_level = self.pitch_env.step(sr, 0.05);
        let base_hz = 55.0 * 2f32.powf(p.tune_st / 12.0);
        let freq = base_hz * (1.0 + pitch_level * 2.0);
        self.phase += freq / sr;
        if self.phase >= 1.0 { self.phase -= 1.0; }
        let sine = (self.phase * TAU).sin();
        // Soft-saturate the sine slightly for body.
        let body = (sine * 1.3).tanh();
        let amp = self.amp_env.step(sr, p.decay_s);
        // Click transient — short noise burst, 5 ms tau, HPF'd via
        // ratio-of-decays trick (subtract slow envelope from fast).
        let click_amp = self.click_env.step(sr, 0.005);
        let click = self.noise.next() * click_amp;
        (body * amp + click) * p.level
    }
}

// ---------------------------------------------------------------------------
// SNARE — noise + two pitched tones with separate envelopes.
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Default)]
pub struct Snare {
    tone_env: OneShotEnv,
    noise_env: OneShotEnv,
    phase_a: f32,
    phase_b: f32,
    noise: Noise,
    noise_bp: OnePoleBp,
    cached_freq: f32,
}

impl Snare {
    pub fn trigger(&mut self, velocity: f32) {
        self.tone_env.trigger(velocity * 0.7);
        self.noise_env.trigger(velocity);
    }
    pub fn is_idle(&self) -> bool {
        self.tone_env.is_idle() && self.noise_env.is_idle()
    }
    pub fn process(&mut self, sr: f32, p: DrumParams) -> f32 {
        let fa = 180.0 * 2f32.powf(p.tune_st / 12.0);
        let fb = fa * 1.5;  // 5th up — Linn / 808 voice trick
        self.phase_a += fa / sr;
        self.phase_b += fb / sr;
        if self.phase_a >= 1.0 { self.phase_a -= 1.0; }
        if self.phase_b >= 1.0 { self.phase_b -= 1.0; }
        let tone = ((self.phase_a * TAU).sin() + (self.phase_b * TAU).sin()) * 0.5;
        let tone_amp = self.tone_env.step(sr, p.decay_s * 0.5);
        // Noise band centred ~2.5 kHz × tune ratio.
        let bp_freq = 2500.0 * 2f32.powf(p.tune_st / 12.0);
        if (bp_freq - self.cached_freq).abs() > 5.0 {
            self.noise_bp.set(sr, bp_freq, 0.9);
            self.cached_freq = bp_freq;
        }
        let n = self.noise.next();
        let bandpassed = self.noise_bp.process(n);
        let n_amp = self.noise_env.step(sr, p.decay_s);
        (tone * tone_amp + bandpassed * n_amp * 1.4) * p.level
    }
}

// ---------------------------------------------------------------------------
// HI-HAT (CLOSED / OPEN) — six square oscs mixed to make metallic
// noise then bandpassed at 8 kHz. Same voice, only decay differs.
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Default)]
pub struct HiHat {
    env: OneShotEnv,
    phases: [f32; 6],
    bp: OnePoleBp,
    cached_freq: f32,
}

impl HiHat {
    pub fn trigger(&mut self, velocity: f32) { self.env.trigger(velocity); }
    pub fn is_idle(&self) -> bool { self.env.is_idle() }
    pub fn process(&mut self, sr: f32, p: DrumParams) -> f32 {
        // 808-style: 6 hi-frequency square waves mixed → metallic noise.
        let base = 800.0 * 2f32.powf(p.tune_st / 12.0);
        let ratios: [f32; 6] = [1.0, 1.4471, 1.892, 2.503, 3.213, 4.157];
        let mut sum = 0.0_f32;
        for (i, r) in ratios.iter().enumerate() {
            self.phases[i] += base * r / sr;
            if self.phases[i] >= 1.0 { self.phases[i] -= 1.0; }
            sum += if self.phases[i] < 0.5 { 1.0 } else { -1.0 };
        }
        let metallic = sum * 0.16;
        // BP around 8 kHz × tune ratio for sizzle.
        let bp_freq = 8000.0 * 2f32.powf(p.tune_st / 12.0);
        if (bp_freq - self.cached_freq).abs() > 50.0 {
            self.bp.set(sr, bp_freq.min(sr * 0.45), 1.4);
            self.cached_freq = bp_freq;
        }
        let band = self.bp.process(metallic);
        let amp = self.env.step(sr, p.decay_s);
        band * amp * p.level
    }
}

// ---------------------------------------------------------------------------
// CLAP — burst of three noise hits 7 ms apart then a longer noise tail.
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Default)]
pub struct Clap {
    burst_env: OneShotEnv,
    tail_env: OneShotEnv,
    burst_count: u32,
    burst_samples_remaining: u32,
    burst_total: u32,
    noise: Noise,
    bp: OnePoleBp,
    cached_freq: f32,
}

impl Clap {
    pub fn trigger(&mut self, velocity: f32) {
        self.burst_env.trigger(velocity);
        self.tail_env.trigger(velocity * 0.45);
        self.burst_count = 0;
        self.burst_samples_remaining = 0;
        self.burst_total = 0;
    }
    pub fn is_idle(&self) -> bool {
        self.burst_env.is_idle() && self.tail_env.is_idle() && self.burst_count >= 3
    }
    pub fn process(&mut self, sr: f32, p: DrumParams) -> f32 {
        let bp_freq = 1300.0 * 2f32.powf(p.tune_st / 12.0);
        if (bp_freq - self.cached_freq).abs() > 5.0 {
            self.bp.set(sr, bp_freq, 1.2);
            self.cached_freq = bp_freq;
        }
        let n = self.noise.next();
        let bandpassed = self.bp.process(n);

        // Three quick bursts at trigger time, ~7 ms apart.
        if self.burst_count < 3 {
            if self.burst_samples_remaining == 0 {
                self.burst_env.trigger(1.0);
                self.burst_total = (sr * 0.007) as u32;
                self.burst_samples_remaining = self.burst_total;
                self.burst_count += 1;
            }
            self.burst_samples_remaining = self.burst_samples_remaining.saturating_sub(1);
        }
        // Tail decays slower → the "open" half of the clap envelope.
        let burst_amp = self.burst_env.step(sr, 0.004);
        let tail_amp = self.tail_env.step(sr, p.decay_s);
        bandpassed * (burst_amp + tail_amp) * p.level
    }
}

// ---------------------------------------------------------------------------
// COWBELL — two square oscs slightly detuned + bandpass. Same Latin-
// percussion trick the 808 uses.
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Default)]
pub struct Cowbell {
    env: OneShotEnv,
    phase_a: f32,
    phase_b: f32,
    bp: OnePoleBp,
    cached_freq: f32,
}

impl Cowbell {
    pub fn trigger(&mut self, velocity: f32) { self.env.trigger(velocity); }
    pub fn is_idle(&self) -> bool { self.env.is_idle() }
    pub fn process(&mut self, sr: f32, p: DrumParams) -> f32 {
        let fa = 540.0 * 2f32.powf(p.tune_st / 12.0);
        let fb = fa * 1.4781;  // classic 808 ratio
        self.phase_a += fa / sr;
        self.phase_b += fb / sr;
        if self.phase_a >= 1.0 { self.phase_a -= 1.0; }
        if self.phase_b >= 1.0 { self.phase_b -= 1.0; }
        let s_a = if self.phase_a < 0.5 { 1.0 } else { -1.0 };
        let s_b = if self.phase_b < 0.5 { 1.0 } else { -1.0 };
        let bp_freq = (fa + fb) * 0.5;
        if (bp_freq - self.cached_freq).abs() > 5.0 {
            self.bp.set(sr, bp_freq, 2.5);
            self.cached_freq = bp_freq;
        }
        let mix = (s_a + s_b) * 0.5;
        let band = self.bp.process(mix);
        let amp = self.env.step(sr, p.decay_s);
        band * amp * p.level
    }
}

// ---------------------------------------------------------------------------
// Voice index — must match the GUI order and the GM drum-key map.
// ---------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VoiceKind {
    Kick = 0,
    Snare = 1,
    HatClosed = 2,
    HatOpen = 3,
    Clap = 4,
    Cowbell = 5,
}

/// Map a MIDI note number to a drum voice. We use **six consecutive
/// white keys** so a player can sweep across the keyboard left-to-
/// right and hit every voice in order — much easier to find than the
/// spread GM Percussion layout that puts the kit on alternating
/// white and black keys.
///
/// Mapping (class within an octave), works in any octave (mod 12):
///   C  →  Kick
///   D  →  Snare
///   E  →  HH Closed
///   F  →  HH Open
///   G  →  Clap
///   A  →  Cowbell
///
/// Black keys (C#, D#, F#, G#, A#) and B fall through to the
/// note-output port for chained Wave/Kubyz bass — bass parts in
/// minor / major / blues all play cleanly without colliding with a
/// drum hit.
pub fn note_to_voice(key: u8) -> Option<VoiceKind> {
    if !(12..=108).contains(&key) { return None; }
    match key % 12 {
        0 => Some(VoiceKind::Kick),       // C
        2 => Some(VoiceKind::Snare),      // D
        4 => Some(VoiceKind::HatClosed),  // E
        5 => Some(VoiceKind::HatOpen),    // F
        7 => Some(VoiceKind::Clap),       // G
        9 => Some(VoiceKind::Cowbell),    // A
        _ => None,
    }
}
