//! Mailbox document `notes`: the transcription as JSON, read-only, so an agent can read what
//! was played (start, end, midi, velocity, string, fret).

use crate::Inner;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

pub fn handle(shared: &Inner, doc: &str, op: &str, _value: Value) -> Result<Value, String> {
    match (doc, op) {
        ("", "list") | ("notes", "list") => Ok(json!([{
            "doc": "notes",
            "get": "the last transcription: mode, capo, max_fret, take_start, duration, tempo, midi_file, notes[{start, end, midi, velocity, string?, fret?}] in song seconds; string 1 = high E",
            "set": "read-only. To transcribe again set the Analyse parameter to 0 and then to 1 (plugin_set_params; it acts on the rise and stays at 1 until set back); the audio is the last take captured while the transport played",
        }])),
        ("notes", "get") => {
            let mut v = shared.record.lock().as_ref().map_or_else(|| json!({"notes": []}), |r| r.to_json());
            v["status"] = json!(shared.status.lock().clone());
            v["captured_seconds"] = json!(shared.captured_secs.load(Ordering::Relaxed));
            v["analysing"] = json!(shared.analysing.load(Ordering::Relaxed));
            Ok(v)
        }
        ("notes", "set") => Err("notes is read-only; set Analyse 0 then 1 to transcribe again".into()),
        _ => Err(format!("Audio2MIDI has one document, \"notes\" (list/get); got {doc:?} {op:?}")),
    }
}
