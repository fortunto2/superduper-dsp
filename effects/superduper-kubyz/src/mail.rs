//! Kubyz's mailbox document (`superduper_synth_core::bus_mail`): the 16
//! harmonic amplitudes and the 3 formant bands, the same atomics the GUI's
//! bar editor writes — so an open window shows an agent's change at once.

use crate::{SharedParamsInner, N_HARMONICS};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

/// The bar editor's ceiling.
const MAX_AMP: f32 = 1.5;

fn floats(v: &Value) -> Option<Vec<f32>> {
    v.as_array()?.iter().map(|x| x.as_f64().map(|f| f as f32)).collect()
}

pub fn handle(shared: &SharedParamsInner, doc: &str, op: &str, value: Value) -> Result<Value, String> {
    let load = |a: &[atomic_float::AtomicF32]| a.iter().map(|x| x.load(Ordering::Relaxed)).collect::<Vec<f32>>();
    match (doc, op) {
        ("", "list") | ("harmonics", "list") => Ok(json!([{
            "doc": "harmonics",
            "get": format!("harmonics[{N_HARMONICS}] amplitudes (0..{MAX_AMP}, harmonic 1 first), formant_bw[3], formant_gain[3]"),
            "set": format!("any of {{harmonics: [up to {N_HARMONICS} amplitudes, 0..{MAX_AMP}], formant_bw: [3], formant_gain: [3]}}; omitted fields stay"),
        }])),
        ("harmonics", "get") => Ok(json!({
            "harmonics": load(&shared.harmonics),
            "formant_bw": load(&shared.formant_bw),
            "formant_gain": load(&shared.formant_gain),
        })),
        ("harmonics", "set") => {
            let mut changed = Vec::new();
            if let Some(h) = value.get("harmonics") {
                let h = floats(h).ok_or("harmonics: a list of numbers")?;
                if h.len() > N_HARMONICS { return Err(format!("harmonics: at most {N_HARMONICS}")); }
                // Unlisted upper harmonics go to 0: a short list means "only these".
                for (i, slot) in shared.harmonics.iter().enumerate() {
                    slot.store(h.get(i).copied().unwrap_or(0.0).clamp(0.0, MAX_AMP), Ordering::Relaxed);
                }
                changed.push("harmonics");
            }
            for (key, atoms) in [("formant_bw", &shared.formant_bw), ("formant_gain", &shared.formant_gain)] {
                if let Some(v) = value.get(key) {
                    let v = floats(v).ok_or(format!("{key}: a list of 3 numbers"))?;
                    if v.len() != 3 { return Err(format!("{key}: exactly 3 values")); }
                    for (slot, x) in atoms.iter().zip(v) { slot.store(x, Ordering::Relaxed); }
                    changed.push(key);
                }
            }
            if changed.is_empty() { return Err("set needs harmonics, formant_bw or formant_gain".into()); }
            Ok(json!({"changed": changed}))
        }
        _ => Err(format!("Kubyz has one document, \"harmonics\" (list/get/set); got {doc:?} {op:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_then_get_and_clamp() {
        let shared = crate::PluginShared::new();
        let s = &shared.inner;
        handle(s, "harmonics", "set", json!({"harmonics": [1.0, 0.0, 9.0]})).expect("set");
        let got = handle(s, "harmonics", "get", Value::Null).expect("get");
        let h: Vec<f64> = got["harmonics"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        assert_eq!(h.len(), N_HARMONICS);
        assert_eq!((h[0], h[1], h[2] as f32, h[3]), (1.0, 0.0, MAX_AMP, 0.0), "clamped, and unlisted ones zeroed");
        assert!(handle(s, "harmonics", "set", json!({"formant_bw": [1.0]})).is_err(), "3 formants or nothing");
    }
}
