//! Kubyz (Bashkir jaw-harp / khomus) physical-model DSP — extracted from the superduper-kubyz plugin
//! so iOS/live2play reuses the exact same engine (the "single core" pattern; see [[wave_osc]]).
//! 16-harmonic additive voice + 3-band formant + a mouth-trajectory modulator.

/// Number of additive harmonics in the jaw-harp model. THE source of this const — the plugin's
/// presets re-export it so the preset tables and the voice agree.
pub const N_HARMONICS: usize = 16;

pub mod trajectory;
pub mod voice;
