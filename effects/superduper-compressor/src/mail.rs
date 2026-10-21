//! Compressor's mailbox document (`superduper_synth_core::bus_mail`): the
//! gain-reduction meter, read-only, so an agent can check "about 6 dB on the
//! peaks, then the least that still holds" (Mixing Secrets ch9) with a number
//! instead of a guess.

use crate::{SharedParamsInner, SCOPE_LEN};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

/// The meter history over its last ~0.7 s, in dB (gain reduction ≤ 0).
fn summary(shared: &SharedParamsInner) -> Value {
    let (mut i, mut o, mut g) = (vec![0.0; SCOPE_LEN], vec![0.0; SCOPE_LEN], vec![0.0; SCOPE_LEN]);
    shared.meter.snapshot_in_order(&mut i, &mut o, &mut g);
    let max_gr = g.iter().fold(0.0f32, |m, x| m.min(*x));
    let mean_gr = g.iter().sum::<f32>() / g.len() as f32;
    let peak = |v: &[f32]| v.iter().fold(f32::NEG_INFINITY, |m, x| m.max(*x));
    json!({
        "gain_reduction_db": shared.gain_reduction_db.load(Ordering::Relaxed),
        "max_gr_db": max_gr,
        "mean_gr_db": mean_gr,
        "in_peak_db": peak(&i),
        "out_peak_db": peak(&o),
        "window_s": 0.7,
    })
}

pub fn handle(shared: &SharedParamsInner, doc: &str, op: &str, _value: Value) -> Result<Value, String> {
    match (doc, op) {
        ("", "list") | ("meter", "list") => Ok(json!([{
            "doc": "meter",
            "get": "gain_reduction_db now; max_gr_db / mean_gr_db, in_peak_db / out_peak_db over the last ~0.7 s (read it a few times while the loudest part plays)",
            "set": "read-only",
        }])),
        ("meter", "get") => Ok(summary(shared)),
        ("meter", "set") => Err("the meter is read-only; change Threshold/Ratio through the host".into()),
        _ => Err(format!("Compressor has one document, \"meter\" (list/get); got {doc:?} {op:?}")),
    }
}
