//! Morph's mailbox document `target` (`superduper_synth_core::bus_mail`).
//!
//! Copy a running Kubyz's timbre onto a voice, agent-side:
//!
//! ```text
//! h = plugin_get(track, "Kubyz", "harmonics")      // {harmonics, formants, ...}
//! plugin_set(track, "Morph", "target", h)          // model := that, Source := Model
//! ```
//!
//! `set` accepts Kubyz's `harmonics` doc as-is (`formant_count`,
//! `formant_bw`, `formant_gain` are ignored when `formants` is present), plus
//! `vox_mix`, `{"capture": true}` (freeze the sidechain now) and
//! `{"captured": <the get's captured object>}` (restore a snapshot).

use crate::targets::{Band, ModelJson, ProfileJson};
use crate::{SharedParamsInner, MODE_NAMES, P_MODE, P_SOURCE, SOURCE_NAMES};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use superduper_synth_core::timbre::{grid_hz, GRID};

const FRAME_MODES: [&str; 3] = ["pass", "harmonic", "envelope"];

fn get(s: &SharedParamsInner) -> Value {
    let p = |i: usize| s.params[i].load(Ordering::Relaxed).round().max(0.0) as usize;
    let cap = s.captured.load();
    let l = &s.live;
    let n = l.n.load(Ordering::Relaxed) as usize;
    let arr = |a: &[atomic_float::AtomicF32]| a.iter().take(n).map(|x| x.load(Ordering::Relaxed)).collect::<Vec<_>>();
    json!({
        "source": SOURCE_NAMES.get(p(P_SOURCE)).copied().unwrap_or("?"),
        "mode": MODE_NAMES.get(p(P_MODE)).copied().unwrap_or("?"),
        "model": ModelJson::from_model(&s.model.load()),
        "captured": if cap.env_valid { json!(ProfileJson::from_profile(&cap)) } else { Value::Null },
        "grid_hz": (0..GRID).map(grid_hz).collect::<Vec<_>>(),
        "live": {
            "f0": l.f0.load(Ordering::Relaxed),
            "voiced": l.voiced.load(Ordering::Relaxed),
            "frame_mode": FRAME_MODES[(l.mode.load(Ordering::Relaxed) as usize).min(2)],
            "sidechain_f0": l.sc_f0.load(Ordering::Relaxed),
            "sidechain_voiced": l.sc_voiced.load(Ordering::Relaxed),
            "sidechain_level_db": l.sc_level_db.load(Ordering::Relaxed),
            "voice_harmonics_db": arr(&l.voice),
            "target_harmonics_db": arr(&l.target),
            "output_harmonics_db": arr(&l.out),
        },
    })
}

fn floats(v: &Value) -> Option<Vec<f32>> {
    v.as_array()?.iter().map(|x| x.as_f64().map(|f| f as f32)).collect()
}

fn set(s: &SharedParamsInner, v: Value) -> Result<Value, String> {
    let mut changed = Vec::new();
    if v.get("harmonics").is_some() || v.get("formants").is_some() || v.get("vox_mix").is_some() {
        let mut m = ModelJson::from_model(&s.model.load());
        if let Some(h) = v.get("harmonics") {
            m.harmonics = floats(h).ok_or("harmonics: a list of numbers")?;
        }
        if let Some(f) = v.get("formants") {
            m.formants = serde_json::from_value::<Vec<Band>>(f.clone()).map_err(|_| "formants: a list of {f, bw?, gain?}")?;
        }
        if let Some(x) = v.get("vox_mix").and_then(Value::as_f64) {
            m.vox_mix = x as f32;
        }
        let model = m.to_model()?;
        // The model now belongs to whatever Target is selected; it holds until
        // Target moves.
        s.model.from_target.store(s.params[crate::P_TARGET].load(Ordering::Relaxed).round() as u32, Ordering::Relaxed);
        s.model.store(&model);
        s.write_param(P_SOURCE, 2.0);
        changed.push("model");
    }
    if let Some(c) = v.get("captured") {
        let p: ProfileJson = serde_json::from_value(c.clone()).map_err(|e| format!("captured: {e}"))?;
        s.captured.store(&p.to_profile()?);
        changed.push("captured");
    }
    if v.get("capture").and_then(Value::as_bool) == Some(true) {
        s.capture_request.store(true, Ordering::Release);
        changed.push("capture requested (next audio block)");
    }
    if changed.is_empty() {
        return Err("set needs harmonics / formants / vox_mix, captured, or capture: true".into());
    }
    Ok(json!({"changed": changed}))
}

pub fn handle(s: &SharedParamsInner, doc: &str, op: &str, value: Value) -> Result<Value, String> {
    match (doc, op) {
        ("", "list") | ("target", "list") => Ok(json!([{
            "doc": "target",
            "get": "source, mode, model {harmonics[16] 0..1.5, formants [{f, bw, gain}], vox_mix}, captured {f0, harmonics_db[40], envelope_db[96] on grid_hz, noise} or null, live {f0, voiced, frame_mode, sidechain_*, voice/target/output_harmonics_db}",
            "set": "Kubyz's harmonics doc as-is ({harmonics, formants}) and/or vox_mix → model, Source := Model; {capture: true} freezes the sidechain; {captured: {...}} restores a snapshot",
        }])),
        ("target", "get") => Ok(get(s)),
        ("target", "set") => set(s, value),
        _ => Err(format!("Morph has one document, \"target\" (list/get/set); got {doc:?} {op:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kubyz_doc_copies_into_the_model() {
        let shared = crate::PluginShared::new();
        let s = &shared.inner;
        s.write_param(P_SOURCE, 0.0);
        let kubyz_doc = json!({
            "harmonics": [1.0, 0.5, 0.25],
            "formant_count": 2,
            "formants": [{"f": 500.0, "bw": 100.0, "gain": 1.0}, {"f": 1500.0, "bw": 200.0, "gain": 0.5}],
            "formant_bw": [100.0, 200.0],
            "formant_gain": [1.0, 0.5],
        });
        handle(s, "target", "set", kubyz_doc).expect("set");
        let got = handle(s, "target", "get", Value::Null).expect("get");
        assert_eq!(got["source"], "Model");
        assert_eq!(got["model"]["harmonics"][1], 0.5);
        assert_eq!(got["model"]["harmonics"][3], 0.0, "unlisted harmonics are zero");
        assert_eq!(got["model"]["formants"].as_array().unwrap().len(), 2);
        assert!(handle(s, "target", "set", json!({})).is_err());
        assert!(handle(s, "target", "set", json!({"formants": []})).is_err());
    }
}
