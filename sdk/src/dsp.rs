//! Common DSP primitives for effect authors.
//!
//! These are intentionally minimal building blocks. Anything complex (FFT,
//! resampling, time-stretching) lives in user effect crates that can pull
//! in their own dependencies.

#![allow(dead_code)]

/// One-pole low-pass filter, useful for envelope smoothing.
#[derive(Default, Clone, Copy)]
pub struct OnePole {
    state: f32,
}

impl OnePole {
    /// `alpha` in [0, 1]. Closer to 1 = slower smoothing.
    pub fn process(&mut self, input: f32, alpha: f32) -> f32 {
        self.state = self.state * alpha + input * (1.0 - alpha);
        self.state
    }

    pub fn reset(&mut self) {
        self.state = 0.0;
    }
}

/// Simple envelope follower with separate attack/release smoothing.
#[derive(Default, Clone, Copy)]
pub struct EnvelopeFollower {
    env: f32,
}

impl EnvelopeFollower {
    pub fn process(&mut self, input: f32, attack: f32, release: f32) -> f32 {
        let target = input.abs();
        let coeff = if target > self.env { attack } else { release };
        self.env = self.env * coeff + target * (1.0 - coeff);
        self.env
    }
}

/// Compute one-pole coefficient from time constant in seconds.
/// `time_const_sec` = how long to reach 1 - 1/e of a target.
#[inline]
pub fn time_to_coeff(time_const_sec: f32, sample_rate: f32) -> f32 {
    if time_const_sec <= 0.0 {
        return 0.0;
    }
    let alpha = (-1.0 / (time_const_sec * sample_rate)).exp();
    alpha
}

/// Soft clip via tanh, scaled by drive amount.
#[inline]
pub fn soft_clip(x: f32, drive: f32) -> f32 {
    (x * (1.0 + drive * 4.0)).tanh()
}

/// Hard clip to [-1, 1].
#[inline]
pub fn hard_clip(x: f32) -> f32 {
    x.clamp(-1.0, 1.0)
}

/// DC blocker (1st-order highpass).
#[derive(Default, Clone, Copy)]
pub struct DcBlocker {
    prev_in: f32,
    prev_out: f32,
}

impl DcBlocker {
    pub fn process(&mut self, x: f32) -> f32 {
        // y[n] = x[n] - x[n-1] + 0.995 * y[n-1]
        let y = x - self.prev_in + 0.995 * self.prev_out;
        self.prev_in = x;
        self.prev_out = y;
        y
    }
}

/// Enable flush-to-zero for denormals on x86_64 / aarch64.
/// Call once at the start of `process` if your DSP can produce denormals.
#[inline]
pub fn denormal_guard() {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        use core::arch::x86_64::*;
        let mxcsr = _mm_getcsr();
        // Bits 15 (FTZ) and 6 (DAZ)
        _mm_setcsr(mxcsr | (1 << 15) | (1 << 6));
    }
    // On aarch64, FTZ is in FPCR. The Rust stable API for this is unstable;
    // most production CLAP plugins on Apple Silicon enable it via inline asm.
    // For now, we rely on the audio host setting it.
}
