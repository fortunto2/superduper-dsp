//! Kubyz's mailbox document (`superduper_synth_core::bus_mail`): the 16
//! harmonic amplitudes and the 1–6 formant bands, the same atomics the GUI's
//! bar editor and formant editor write — so an open window shows an agent's
//! change at once.
//!
//! `formant_bw` / `formant_gain` keep their original meaning (one value per
//! live band — 3 unless bands were added). `formants` is the full form:
//! a list of `{f, bw, gain}` whose length *is* the band count.

use crate::formants::{self, MAX_FORMANTS, MIN_FORMANTS};
use crate::{SharedParamsInner, N_HARMONICS};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

/// The bar editor's ceiling.
const MAX_AMP: f32 = 1.5;

/// Numbers that stay finite as f32 (1e39 would be inf).
fn floats(v: &Value) -> Option<Vec<f32>> {
    v.as_array()?.iter().map(|x| x.as_f64().filter(|f| f.is_finite() && f.abs() <= 1e6).map(|f| f as f32)).collect()
}

pub fn handle(shared: &SharedParamsInner, doc: &str, op: &str, value: Value) -> Result<Value, String> {
    let load = |a: &[atomic_float::AtomicF32]| a.iter().map(|x| x.load(Ordering::Relaxed)).collect::<Vec<f32>>();
    match (doc, op) {
        ("preset", op) => {
            let all = crate::presets();
            let names: Vec<&str> = all.iter().map(|p| p.name).collect();
            superduper_synth_core::bus_mail::preset_request(op, &value, &names, shared.active_preset.load(Ordering::Relaxed), |i| crate::apply_preset_idx(shared, i))
        }
        ("", "list") | ("harmonics", "list") => Ok(json!([superduper_synth_core::bus_mail::preset_list_entry(), {
            "doc": "harmonics",
            "get": format!("harmonics[{N_HARMONICS}] amplitudes (0..{MAX_AMP}, harmonic 1 first), formant_count, formants[{{f Hz, bw Hz, gain linear}}] (one per live band), formant_bw[count], formant_gain[count]"),
            "set": format!("any of {{harmonics: [up to {N_HARMONICS} amplitudes, 0..{MAX_AMP}], formants: [{MIN_FORMANTS}..{MAX_FORMANTS} × {{f, bw?, gain?}}] (sets the band count), formant_bw: [count], formant_gain: [count]}}; omitted fields stay"),
        }])),
        ("harmonics", "get") => {
            let set = formants::load(shared);
            let live = &set.bands[..set.count];
            Ok(json!({
                "harmonics": load(&shared.harmonics),
                "formant_count": set.count,
                "formants": live.iter().map(|b| json!({"f": b.f, "bw": b.bw, "gain": b.gain})).collect::<Vec<_>>(),
                "formant_bw": live.iter().map(|b| b.bw).collect::<Vec<_>>(),
                "formant_gain": live.iter().map(|b| b.gain).collect::<Vec<_>>(),
            }))
        }
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
            if let Some(list) = value.get("formants") {
                set_formants(shared, list)?;
                changed.push("formants");
            }
            let live = formants::count(shared);
            for (key, atoms) in [("formant_bw", &shared.formant_bw), ("formant_gain", &shared.formant_gain)] {
                if let Some(v) = value.get(key) {
                    let v = floats(v).ok_or(format!("{key}: a list of {live} numbers"))?;
                    if v.len() != live { return Err(format!("{key}: exactly {live} values (one per live band)")); }
                    // Same rule as set_formants: a non-finite or absurd value would put
                    // NaN through the formant bank (0 · inf) and out of the plugin.
                    let ok = |x: &f32| x.is_finite() && *x >= 0.0 && *x <= if key == "formant_bw" { 20_000.0 } else { 16.0 };
                    if !v.iter().all(ok) { return Err(format!("{key}: finite, ≥ 0 (bw ≤ 20000 Hz, gain ≤ 16)")); }
                    for (slot, x) in atoms.iter().zip(v) { slot.store(x, Ordering::Relaxed); }
                    changed.push(key);
                }
            }
            if changed.is_empty() { return Err("set needs harmonics, formants, formant_bw or formant_gain".into()); }
            Ok(json!({"changed": changed}))
        }
        _ => Err(format!("Kubyz has one document, \"harmonics\" (list/get/set); got {doc:?} {op:?}")),
    }
}

/// `formants: [{f, bw?, gain?}, …]` — replaces the live set; the list length
/// becomes the band count. Missing bw / gain keep the slot's current value.
fn set_formants(shared: &SharedParamsInner, list: &Value) -> Result<(), String> {
    let items = list.as_array().ok_or("formants: a list of {f, bw, gain}")?;
    if !(MIN_FORMANTS..=MAX_FORMANTS).contains(&items.len()) {
        return Err(format!("formants: {MIN_FORMANTS}..={MAX_FORMANTS} bands"));
    }
    let mut set = formants::load(shared);
    for (b, item) in set.bands.iter_mut().zip(items) {
        let num = |k: &str| item.get(k).and_then(Value::as_f64).map(|x| x as f32);
        b.f = num("f").ok_or("formants: every band needs f (Hz)")?;
        b.bw = num("bw").unwrap_or(b.bw);
        b.gain = num("gain").unwrap_or(b.gain);
        if !b.is_valid() {
            return Err("formants: f, bw, gain finite and bw > 0".into());
        }
    }
    set.count = items.len();
    set.clamp();
    formants::store(shared, &set);
    Ok(())
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

    #[test]
    fn formants_list_sets_the_count_and_round_trips() {
        let shared = crate::PluginShared::new();
        let s = &shared.inner;
        let five = json!({"formants": [
            {"f": 300.0, "bw": 80.0, "gain": 1.0}, {"f": 900.0}, {"f": 2200.0},
            {"f": 3100.0, "bw": 400.0, "gain": 0.5}, {"f": 9000.0, "bw": 500.0, "gain": 0.4},
        ]});
        handle(s, "harmonics", "set", five).expect("set five");
        let got = handle(s, "harmonics", "get", Value::Null).expect("get");
        assert_eq!(got["formant_count"], 5);
        assert_eq!(got["formants"][0]["f"], 300.0);
        assert_eq!(got["formants"][4]["f"], 6000.0, "clamped to the free-band ceiling");
        assert_eq!(got["formant_bw"].as_array().unwrap().len(), 5);
        // The legacy per-band arrays now want 5 values.
        assert!(handle(s, "harmonics", "set", json!({"formant_gain": [1.0, 1.0, 1.0]})).is_err());
        handle(s, "harmonics", "set", json!({"formant_gain": [1.0, 0.9, 0.8, 0.7, 0.6]})).expect("5 gains");
        assert!(handle(s, "harmonics", "set", json!({"formants": []})).is_err(), "at least one band");
        assert!(handle(s, "harmonics", "set", json!({"formants": vec![json!({"f": 500.0}); 7]})).is_err(), "at most six");
    }
}
