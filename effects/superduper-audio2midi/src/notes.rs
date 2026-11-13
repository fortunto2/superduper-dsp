//! The transcription as the audio thread plays it back, and as the GUI, the state and an agent
//! see it.

use crate::{Note, Settings};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

/// One analysis result, in song time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub settings: Settings,
    /// Song position (s) the analysed audio started at; note times already include it.
    pub take_start: f64,
    pub duration: f64,
    pub tempo: f64,
    pub notes: Vec<Note>,
    #[serde(default)]
    pub midi_path: Option<PathBuf>,
    #[serde(default)]
    pub json_path: Option<PathBuf>,
}

impl Record {
    pub fn is_guitar(&self) -> bool {
        self.settings.mode == crate::Mode::Guitar
    }

    /// What an agent reads: the transcription in plain terms.
    pub fn to_json(&self) -> Value {
        json!({
            "mode": if self.is_guitar() { "guitar" } else { "any" },
            "capo": self.settings.capo,
            "max_fret": self.settings.max_fret,
            "onset_threshold": self.settings.onset_thr,
            "frame_threshold": self.settings.frame_thr,
            "take_start": self.take_start,
            "duration": self.duration,
            "tempo": self.tempo,
            "midi_file": self.midi_path,
            "notes": self.notes.iter().map(|n| {
                let mut o = json!({"start": n.start, "end": n.end, "midi": n.midi, "velocity": n.velocity});
                if let (Some(s), Some(f)) = (n.string, n.fret) { o["string"] = s.into(); o["fret"] = f.into(); }
                o
            }).collect::<Vec<_>>(),
        })
    }
}

/// A note-on or note-off at a song time. `vel == 0` is a note-off.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ev {
    pub t: f64,
    pub key: u8,
    pub ch: u8,
    pub vel: u8,
}

/// Events sorted by time, offs before ons at the same instant. Built off the audio thread.
#[derive(Debug, Default)]
pub struct NoteSet {
    pub ev: Vec<Ev>,
}

impl NoteSet {
    pub fn from_notes(notes: &[Note]) -> Self {
        let mut ev = Vec::with_capacity(notes.len() * 2);
        for n in notes {
            let ch = superduper_transcribe::midi::channel_of(n);
            ev.push(Ev { t: n.start, key: n.midi.min(127), ch, vel: n.velocity.clamp(1, 127) });
            ev.push(Ev { t: n.end.max(n.start + 0.02), key: n.midi.min(127), ch, vel: 0 });
        }
        ev.sort_by(|a, b| a.t.total_cmp(&b.t).then((a.vel != 0).cmp(&(b.vel != 0))));
        Self { ev }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offs_sort_before_ons_at_the_same_time() {
        let mk = |s, e, m| Note { start: s, end: e, midi: m, vel: 0.5, velocity: 80, string: None, fret: None };
        let set = NoteSet::from_notes(&[mk(1.0, 2.0, 60), mk(0.0, 1.0, 60)]);
        let seq: Vec<(f64, bool)> = set.ev.iter().map(|e| (e.t, e.vel != 0)).collect();
        assert_eq!(seq, vec![(0.0, true), (1.0, false), (1.0, true), (2.0, false)]);
    }

    #[test]
    fn json_names_the_string_and_fret() {
        let n = Note { start: 0.5, end: 1.0, midi: 52, vel: 0.5, velocity: 80, string: Some(5), fret: Some(7) };
        let r = Record { settings: Settings::default(), take_start: 0.0, duration: 2.0, tempo: 120.0, notes: vec![n], midi_path: None, json_path: None };
        let v = r.to_json();
        assert_eq!(v["notes"][0]["string"], 5);
        assert_eq!(v["notes"][0]["fret"], 7);
    }
}
