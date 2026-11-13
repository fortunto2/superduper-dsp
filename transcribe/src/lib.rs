//! Audio to MIDI for any pitched instrument, with a guitar mode that names the string and fret.
//!
//! Basic Pitch (Spotify, Apache-2.0) runs as an embedded ONNX model on tract, pure Rust. Two
//! decoders read its posteriors:
//!
//! * [`Mode::Any`]: Basic Pitch's own note creation, polyphonic notes for anything pitched;
//! * [`Mode::Guitar`]: strings, frets under tuning + capo, cheapest hand path, re-plucks.
//!
//! ```no_run
//! use superduper_transcribe::{Settings, Transcriber};
//! let t = Transcriber::new().unwrap();
//! let a = t.transcribe(&[0.0; 48000], 48000, &Settings::default(), |_| true).unwrap().unwrap();
//! println!("{} notes", a.notes.len());
//! ```

pub mod basic_pitch;
pub mod guitar;
pub mod midi;
pub mod model;
pub mod resample;

use serde::{Deserialize, Serialize};

pub use basic_pitch::AnyParams;
pub use guitar::GuitarParams;
pub use model::Posteriors;

/// One transcribed note. Times are seconds from the start of the analysed audio.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub start: f64,
    pub end: f64,
    pub midi: u8,
    /// Raw strength: mean frame activation (Any) or onset activation (Guitar), 0..1.
    pub vel: f32,
    /// MIDI velocity 1..=127 derived from `vel` the way each mode's reference does.
    pub velocity: u8,
    /// Guitar mode: string 1 (high E) .. 6 (low E).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub string: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fret: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Mode {
    #[default]
    Any,
    Guitar,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub mode: Mode,
    /// Guitar: capo fret, 0 = none.
    pub capo: u8,
    /// Guitar: highest fret a note may use.
    pub max_fret: u8,
    /// Onset threshold (Any: `--onset-threshold`, Guitar: `on_thr`).
    pub onset_thr: f32,
    /// Frame threshold (Any: `--frame-threshold`, Guitar: `note_thr`).
    pub frame_thr: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { mode: Mode::Any, capo: 0, max_fret: 19, onset_thr: 0.5, frame_thr: 0.3 }
    }
}

#[derive(Debug, Clone)]
pub struct Analysis {
    pub notes: Vec<Note>,
    /// Length of the analysed audio, seconds.
    pub seconds: f64,
}

/// Decode posteriors with the chosen mode.
pub fn decode(post: &Posteriors, s: &Settings) -> Vec<Note> {
    match s.mode {
        Mode::Any => basic_pitch::decode_any(
            post,
            &AnyParams { onset_thr: s.onset_thr, frame_thr: s.frame_thr, ..AnyParams::default() },
        ),
        Mode::Guitar => guitar::decode_guitar(
            post,
            &GuitarParams {
                on_thr: s.onset_thr,
                note_thr: s.frame_thr,
                capo: s.capo,
                max_fret: s.max_fret,
                ..GuitarParams::default()
            },
        ),
    }
}

/// The loaded model. Building one costs ~100 ms, so keep it around.
pub struct Transcriber {
    model: model::Model,
}

impl Transcriber {
    pub fn new() -> Result<Self, String> {
        Ok(Self { model: model::Model::load().map_err(|e| format!("model: {e}"))? })
    }

    /// Posteriors of mono audio at any sample rate (resampled to 22050 Hz first).
    pub fn posteriors(
        &self,
        mono: &[f32],
        sample_rate: u32,
        progress: impl FnMut(f32) -> bool,
    ) -> Result<Option<Posteriors>, String> {
        let a22 = resample::resample(mono, sample_rate, model::SAMPLE_RATE);
        self.model.posteriors(&a22, progress).map_err(|e| format!("inference: {e}"))
    }

    /// Audio to notes. `progress` takes 0..1 and returns `false` to cancel (result `None`).
    pub fn transcribe(
        &self,
        mono: &[f32],
        sample_rate: u32,
        settings: &Settings,
        progress: impl FnMut(f32) -> bool,
    ) -> Result<Option<Analysis>, String> {
        let Some(post) = self.posteriors(mono, sample_rate, progress)? else { return Ok(None) };
        Ok(Some(Analysis { notes: decode(&post, settings), seconds: mono.len() as f64 / sample_rate.max(1) as f64 }))
    }
}
