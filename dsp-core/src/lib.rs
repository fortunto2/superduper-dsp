//! SuperDuper DSP core: the primitives shared by synth-core (every plugin), the loudness meter
//! and the restoration DSP, so each lives in exactly one place.
//!
//! - [`Biquad`]: RBJ cookbook biquad, DF2T.
//! - [`LatencyDelay`]: fixed integer-sample delay for latency-matched dry paths.
//! - [`spectral`] (feature `stft`): the streaming STFT overlap-add processor on realfft.

mod biquad;
mod delay;
#[cfg(feature = "stft")]
pub mod spectral;

pub use biquad::Biquad;
pub use delay::LatencyDelay;
