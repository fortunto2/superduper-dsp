//! Agent access over the bus mailbox: the `position` document — where the
//! track is in the room, in the terms a mix decision uses (x, y, z, spread,
//! distance, the layout and its speakers, the gains the last block used).
//! Parameters themselves also go through the generic `params` doc; this one
//! reads back what the placement *means*.

use crate::dsp::Layout;
use crate::{SharedParamsInner, P_DISTANCE, P_LAYOUT, P_SPREAD, P_X, P_Y, P_Z};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

pub fn handle(shared: &SharedParamsInner, doc: &str, op: &str, value: Value) -> Result<Value, String> {
    let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
    match (doc, op) {
        ("", "list") | ("position", "list") => Ok(json!([{
            "doc": "position",
            "get": "x (−1 left … 1 right), y (−1 back … 1 front), z (0 … 1 overhead, 7.1.4), spread, distance, azimuth_deg, layout, layouts[], speakers[{channel, name, azimuth_deg, height, gain}]",
            "set": "any of {x, y, z, spread, distance, layout (name or index)}; omitted fields stay",
        }])),
        ("position", "get") => {
            let layout = Layout::from_param(load(P_LAYOUT));
            let speakers: Vec<Value> = layout.speakers().iter().map(|&(c, name, az, height)| json!({
                "channel": c, "name": name, "azimuth_deg": az, "height": height,
                "gain": shared.live_gain[c].load(Ordering::Relaxed),
            })).collect();
            Ok(json!({
                "x": load(P_X), "y": load(P_Y), "z": load(P_Z),
                "spread": load(P_SPREAD), "distance": load(P_DISTANCE),
                "azimuth_deg": crate::dsp::azimuth_deg(load(P_X), load(P_Y)),
                "layout": crate::dsp::LAYOUT_NAMES[load(P_LAYOUT).round().clamp(0.0, 4.0) as usize],
                "layouts": crate::dsp::LAYOUT_NAMES,
                "speakers": speakers,
            }))
        }
        ("position", "set") => {
            let mut set = |i: usize, v: f32, lo: f32, hi: f32| {
                shared.params[i].store(v.clamp(lo, hi), Ordering::Relaxed);
                shared.dirty_params[i].store(true, Ordering::Relaxed);
            };
            let mut changed = Vec::new();
            for (key, idx, lo, hi) in [("x", P_X, -1.0, 1.0), ("y", P_Y, -1.0, 1.0), ("z", P_Z, 0.0, 1.0), ("spread", P_SPREAD, 0.0, 1.0), ("distance", P_DISTANCE, 0.0, 1.0)] {
                if let Some(v) = value.get(key) {
                    let v = v.as_f64().filter(|f| f.is_finite()).ok_or(format!("{key}: a number"))? as f32;
                    set(idx, v, lo, hi);
                    changed.push(key);
                }
            }
            if let Some(l) = value.get("layout") {
                let idx = if let Some(i) = l.as_u64() {
                    (i as usize).min(crate::dsp::LAYOUT_NAMES.len() - 1)
                } else if let Some(s) = l.as_str() {
                    let key = s.trim().to_lowercase();
                    crate::dsp::LAYOUT_NAMES.iter().position(|n| n.to_lowercase() == key)
                        .ok_or(format!("layout: one of {:?}", crate::dsp::LAYOUT_NAMES))?
                } else {
                    return Err("layout: a name or an index".into());
                };
                set(P_LAYOUT, idx as f32, 0.0, 4.0);
                changed.push("layout");
            }
            if changed.is_empty() {
                return Err("set needs any of x, y, z, spread, distance, layout".into());
            }
            Ok(json!({ "changed": changed, "azimuth_deg": crate::dsp::azimuth_deg(load(P_X), load(P_Y)) }))
        }
        _ => Err(format!("no document {doc:?} here; documents: position")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_by_name_and_read_back() {
        let shared = crate::PluginShared::new();
        let r = handle(&shared.inner, "position", "set", json!({"x": 0.5, "y": 0.0, "layout": "7.1.4", "z": 1.0})).unwrap();
        assert!((r["azimuth_deg"].as_f64().unwrap() - 90.0).abs() < 1e-4, "{r}");
        let g = handle(&shared.inner, "position", "get", json!({})).unwrap();
        assert_eq!(g["layout"], "7.1.4");
        assert_eq!(g["speakers"].as_array().unwrap().len(), 12);
        assert!(handle(&shared.inner, "position", "set", json!({"layout": "9.1"})).is_err());
        assert!(handle(&shared.inner, "position", "set", json!({})).is_err());
    }
}
