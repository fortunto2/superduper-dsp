//! Reusable DSP building blocks shared across SuperDuper effects.
//!
//! Each block is `#[derive(Default)]`-constructible and exposes a single
//! `process` method that takes a sample (or a stereo pair) plus runtime
//! parameters. None of them allocate — they are safe to call from the
//! audio thread.

// ---------------------------------------------------------------------------
// SmoothedParam — one-pole interpolator for CLAP parameter slews.
//
// Reading an `AtomicF32` per sample is fine, but using the raw value
// per sample produces audible zipper noise when the user crank a knob.
// The standard fix is to slew toward the new value with a one-pole filter.
// ---------------------------------------------------------------------------

#[derive(Default, Copy, Clone)]
pub struct SmoothedParam {
    current: f32,
}

impl SmoothedParam {
    /// Create with an initial value (use the CLAP default).
    pub fn new(initial: f32) -> Self {
        Self { current: initial }
    }

    /// Update toward `target` with a 5 ms time constant at the given sample
    /// rate. Returns the new (slewed) value.
    #[inline]
    pub fn step(&mut self, target: f32, sr: f32) -> f32 {
        // ~5 ms one-pole. At 48 kHz coef ≈ 0.99584.
        let coef = (-1.0 / (0.005 * sr)).exp();
        self.current = target + (self.current - target) * coef;
        self.current
    }

    /// Snap immediately (used at activate() time to avoid a 5 ms fade-in
    /// from the default value to whatever the host loaded from project state).
    pub fn snap(&mut self, value: f32) {
        self.current = value;
    }

    pub fn current(&self) -> f32 {
        self.current
    }
}

// ---------------------------------------------------------------------------
// DcBlocker — first-order high-pass at ~5 Hz.
//
// Critical inside any reverb feedback loop: DC offset (from analog input
// drift, or numerical accumulation through long delays) gets multiplied
// every loop iteration, eventually drowning the audible tail in a constant
// hum that the user perceives as "the reverb died". The blocker is cheap
// (one multiply, one subtract, one add) and harmless for audio content.
// ---------------------------------------------------------------------------

#[derive(Default, Copy, Clone)]
pub struct DcBlocker {
    x_prev: f32,
    y_prev: f32,
}

impl DcBlocker {
    /// Standard formula: y[n] = x[n] - x[n-1] + R * y[n-1], R = 0.995.
    /// Cuts off at roughly sr * (1 - R) / (2π) ≈ 38 Hz at 48 kHz, plenty
    /// gentle to be inaudible.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        const R: f32 = 0.995;
        let y = x - self.x_prev + R * self.y_prev;
        self.x_prev = x;
        self.y_prev = y;
        y
    }
}

// ---------------------------------------------------------------------------
// Tilt — single-shelf brightness control, ±6 dB at ±1.0.
// ---------------------------------------------------------------------------

#[derive(Default, Copy, Clone)]
pub struct Tilt {
    lp: f32,
}

impl Tilt {
    /// One-pole LPF at 1.5 kHz splits the band; tilt > 0 boosts highs,
    /// tilt < 0 boosts lows. `tilt` clamped to [-1, +1] internally.
    #[inline]
    pub fn process(&mut self, x: f32, sr: f32, tilt: f32) -> f32 {
        let tilt = tilt.clamp(-1.0, 1.0);
        let coef = (-core::f32::consts::TAU * 1500.0 / sr).exp();
        self.lp = x * (1.0 - coef) + self.lp * coef;
        let low = self.lp;
        let high = x - low;
        let gain_hi = 10f32.powf(tilt * 6.0 / 20.0);
        let gain_lo = 10f32.powf(-tilt * 6.0 / 20.0);
        low * gain_lo + high * gain_hi
    }
}

// ---------------------------------------------------------------------------
// Ducker — peak-envelope-driven gain reducer with asymmetric attack/release.
//
// Used to drive sidechain ducking inside reverbs / delays / saturators —
// the classic vocal-bus pattern (key signal turns wet down so the dry
// sits in the front of the mix). Same primitive used by superduper-reverb
// and superduper-supermass.
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Ducker {
    envelope: f32,
}

impl Ducker {
    /// `amount_db` is the maximum attenuation (positive number) at full key.
    /// Returns a linear gain ∈ (0, 1] to apply to the wet path.
    #[inline]
    pub fn process(
        &mut self,
        key_l: f32,
        key_r: f32,
        sr: f32,
        amount_db: f32,
        attack_ms: f32,
        release_ms: f32,
    ) -> f32 {
        let rectified = key_l.abs().max(key_r.abs());
        let coef = if rectified > self.envelope {
            (-1.0 / (attack_ms.max(0.1) * 0.001 * sr)).exp()
        } else {
            (-1.0 / (release_ms.max(0.1) * 0.001 * sr)).exp()
        };
        self.envelope = rectified + (self.envelope - rectified) * coef;

        if amount_db <= 0.001 {
            return 1.0;
        }
        let drive = (self.envelope * 4.0).min(1.0);
        10f32.powf(-(amount_db * drive) / 20.0)
    }

    /// Read the current envelope (for metering / debugging). Not used in
    /// the audio path.
    pub fn envelope(&self) -> f32 {
        self.envelope
    }
}
