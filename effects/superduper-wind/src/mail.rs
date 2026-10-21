//! Agent access over the bus mailbox: the `preset` document — recall by
//! index or name. The Preset *parameter* is a label (CLAP parameters are
//! independent; see `synth_core::bus_mail::preset_request`), so this is the
//! one way an agent recalls a preset: `plugin_set(track, "…", "preset",
//! {"name": "…"})`. Parameters themselves go through the generic `params` doc.

use crate::SharedParamsInner;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

pub fn handle(shared: &SharedParamsInner, doc: &str, op: &str, value: Value) -> Result<Value, String> {
    match (doc, op) {
        ("", "list") => Ok(json!([superduper_synth_core::bus_mail::preset_list_entry()])),
        ("preset", op) => {
            let names: Vec<&str> = crate::PRESETS.iter().map(|p| p.name).collect();
            superduper_synth_core::bus_mail::preset_request(op, &value, &names, shared.active_preset.load(Ordering::Relaxed), |i| crate::apply_preset_idx(shared, i))
        }
        _ => Err(format!("no document {doc:?} here; documents: preset")),
    }
}
