//! Shared DSP code carried over from the original `rust-synth` project.
//!
//! This crate is the home for voice builders, the Supermassive-style reverb,
//! and the math utilities (harmony / rhythm / motif / etc.) that every
//! SuperDuper synth plugin will eventually share.
//!
//! For now we expose the bare minimum needed to ship the standalone
//! Supermass reverb effect. Voices and math will follow as the Ambient and
//! Pad plugins land.

pub mod analysis;
pub mod dsp_blocks;
pub mod formant;
pub mod formant_fx; // formant articulator: tracker + resonators + trajectory (superduper-formant's DSP)
pub mod formant_track; // live F1/F2/F3 estimator (drives superduper-formant's Follow mode)
pub mod granular; // real-time granular cloud + freeze (drives superduper-granular)
pub mod paulstretch; // extreme time-stretch smear, random-phase STFT (drives superduper-stretch)
pub mod linphase;
pub mod loudness;
pub mod melody; // offline note segmentation + correction planning (sdsp-tune, future editor)
pub mod nam;
pub mod pitch;
pub mod pitch_engine; // picks PSOLA or the phase vocoder by measuring the material (shared by superduper-pitch and superduper-tune)
pub mod pvoc; // STFT phase vocoder — the polyphony-capable engine, and PSOLA's fallback on material with no glottal epoch
pub mod psola; // TD-PSOLA pitch/formant shifter (extracted from superduper-pitch; shared with superduper-tune + iOS)
pub mod drumlib; // drum one-shot library: class from name or sound, pad mapping (+ folder scan behind `sfz-load`)
pub mod bus_mail; // the way back in: per-instance mailbox next to the bus (agents read and set non-param state)
pub mod mixmap; // who fights whom in which band, from bus spectra (Spectrum Mix view, bus gateway)
pub mod sfz; // SFZ multisample engine: parser, regions, voices (+ loader behind `sfz-load`) — SuperDuper Piano
pub mod spectral;
pub mod supermass;
pub mod swiftf0; // SwiftF0 neural pitch tracker, pure-Rust inference (option in superduper-tune)
pub mod user_preset;
pub mod wav;
pub mod wave_osc; // wavetable oscillator/voice (extracted from superduper-wave so it reaches iOS too)
pub mod drum_voices; // 6 analog drum voices (extracted from superduper-drum)
pub mod kubyz; // Bashkir jaw-harp additive model (extracted from superduper-kubyz)
pub mod scale; // key/scale quantiser (Tune's rule, shared so superduper-double can snap)
pub mod doubler; // vocal doubler: per-voice drifting PSOLA heads over one tracker (superduper-double)
pub mod timbre; // harmonic + envelope timbre analysis and morph gains (superduper-morph)

#[cfg(feature = "gui")]
pub mod gui;
#[cfg(feature = "gui")]
pub mod bus_key; // reacting to the bus key: span level, factor, duck gain, smoothed envelope (Reverb/Delay/Filter/Vocal)
#[cfg(feature = "gui")]
pub mod effect; // the front half of every effect's process(): events, bypass that still feeds the scope
#[cfg(feature = "gui")]
pub mod mix_view; // the mix map any plugin window can open (Spectrum full size, others from the top bar)
