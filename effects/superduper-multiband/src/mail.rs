//! Multiband's mailbox document (`superduper_synth_core::bus_mail`): the
//! per-band gain-reduction meter, read-only, so an agent can check "the low
//! band takes 3 dB on the kick, the mids nothing" with numbers.

use crate::dsp::{band_range, MAX_BANDS};
use crate::{settings_from, SharedParamsInner};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

fn summary(shared: &SharedParamsInner) -> Value {
    let s = settings_from(&shared.params);
    let m = &shared.meters;
    let bands: Vec<Value> = (0..s.bands.min(MAX_BANDS)).map(|b| {
        let (lo, hi) = band_range(s.bands, &s.xover_hz, b);
        json!({
            "band": b + 1,
            "lo_hz": lo.round(),
            "hi_hz": hi.round(),
            "gain_reduction_db": m.gr_db[b].load(Ordering::Relaxed),
            "max_gr_db": m.max_gr_db[b].load(Ordering::Relaxed),
            "detector_db": m.detector_db[b].load(Ordering::Relaxed),
            "key_db": m.key_db[b].load(Ordering::Relaxed),
            "key_amount_pct": s.band[b].key * 100.0,
            "bypass": s.band[b].bypass,
            "solo": s.band[b].solo,
        })
    }).collect();
    json!({ "bands": bands, "window_s": 0.7, "key_track": shared.scope.key_track.lock().clone() })
}

pub fn handle(shared: &SharedParamsInner, doc: &str, op: &str, _value: Value) -> Result<Value, String> {
    match (doc, op) {
        ("", "list") | ("meter", "list") => Ok(json!([{
            "doc": "meter",
            "get": "per band: range, gain_reduction_db now, max_gr_db over ~0.7 s, detector_db, the bus key's level in that band (key_db) and how much it drives the band (key_amount_pct)",
            "set": "read-only",
        }])),
        ("meter", "get") => Ok(summary(shared)),
        ("meter", "set") => Err("the meter is read-only; change B<n> Threshold/Ratio/Key through \"params\"".into()),
        _ => Err(format!("Multiband has one document, \"meter\" (list/get); got {doc:?} {op:?}")),
    }
}
