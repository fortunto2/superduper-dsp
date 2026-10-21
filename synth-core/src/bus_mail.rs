//! The bus's way back in: a mailbox per plugin instance, so an agent can read
//! and change what is not a CLAP parameter — Wave's drawn wavetable, Kubyz's
//! harmonics — the same way it reads the bus.
//!
//! One request at a time per instance, as files next to the bus:
//! `mail/<instance>.req.json` (written to a temp name and renamed, so the
//! plugin never reads half of it) and `mail/<instance>.res.json` back. The
//! plugin's bus publisher thread picks requests up (it wakes every 50 ms
//! anyway); nothing here touches the audio thread.
//!
//! A plugin names its documents ("wavetable", "harmonics") and answers
//! `op` = "list" | "get" | "set" for each; `doc` = "" with "list" asks which
//! documents it has.
//!
//! Files, not a socket or an IPC crate, on purpose: agent calls come about
//! once a second, and this survives a crashed host, needs no extra thread in
//! the DAW and nothing to install. If the bus ever has to carry a stream of
//! events (renames, notes, hundreds a second), move both it and this mailbox
//! to iceoryx2 (cross-process pub/sub + request/response over shared memory).

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub doc: String,
    pub op: String,
    #[serde(default)]
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    pub ok: bool,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub error: Option<String>,
}

/// Next to the bus file (`SUPERDUPER_BUS` moves both, as the bus tests do).
pub fn mail_dir() -> PathBuf {
    std::env::var_os("SUPERDUPER_BUS")
        .map(|p| PathBuf::from(p).with_file_name("mail"))
        .unwrap_or_else(|| std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".superduper-dsp").join("mail"))
}

fn req_path(instance: u64) -> PathBuf { mail_dir().join(format!("{instance:016x}.req.json")) }
fn res_path(instance: u64) -> PathBuf { mail_dir().join(format!("{instance:016x}.res.json")) }

fn write_atomic(path: &PathBuf, text: &str) -> std::io::Result<()> {
    if let Some(d) = path.parent() { std::fs::create_dir_all(d)?; }
    // A temp name of our own: two writers sharing one ".tmp" could rename
    // each other's half-written file.
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let tmp = path.with_extension(format!("{}-{}.tmp", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Plugin side: take the pending request for this instance, if any.
pub fn take_request(instance: u64) -> Option<Request> {
    let p = req_path(instance);
    let text = std::fs::read_to_string(&p).ok()?;
    let _ = std::fs::remove_file(&p);
    serde_json::from_str(&text).ok()
}

/// Plugin side: answer a request.
pub fn reply(instance: u64, id: u64, result: Result<Value, String>) {
    let r = match result {
        Ok(value) => Reply { id, ok: true, value, error: None },
        Err(e) => Reply { id, ok: false, value: Value::Null, error: Some(e) },
    };
    if let Ok(text) = serde_json::to_string(&r) { let _ = write_atomic(&res_path(instance), &text); }
}

/// Agent side: send one request and wait for its reply.
pub fn call(instance: u64, doc: &str, op: &str, value: Value, timeout: Duration) -> Result<Value, String> {
    let id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64);
    let req = Request { id, doc: doc.into(), op: op.into(), value };
    write_atomic(&req_path(instance), &serde_json::to_string(&req).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(res_path(instance)) {
            if let Ok(r) = serde_json::from_str::<Reply>(&text) {
                if r.id == id {
                    let _ = std::fs::remove_file(res_path(instance));
                    return if r.ok { Ok(r.value) } else { Err(r.error.unwrap_or_default()) };
                }
            }
        }
        if start.elapsed() > timeout {
            // Withdraw the request only if it is still ours (another caller's
            // may have replaced it since).
            if let Ok(text) = std::fs::read_to_string(req_path(instance)) {
                if serde_json::from_str::<Request>(&text).is_ok_and(|r| r.id == id) { let _ = std::fs::remove_file(req_path(instance)); }
            }
            return Err("no answer: the host is not running, the plugin has no mailbox, or another caller's request replaced this one (one at a time per instance)".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_reply_round_trip() {
        let dir = std::env::temp_dir().join(format!("sdsp-mail-{}", std::process::id()));
        std::env::set_var("SUPERDUPER_BUS", dir.join("bus"));
        let inst = 0xabc;
        // The "plugin": answers whatever arrives, from another thread.
        let t = std::thread::spawn(move || {
            for _ in 0..200 {
                if let Some(r) = take_request(inst) {
                    reply(inst, r.id, if r.op == "get" { Ok(serde_json::json!({"doc": r.doc})) } else { Err("read-only".into()) });
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let v = call(inst, "wavetable", "get", Value::Null, Duration::from_secs(2)).expect("answered");
        assert_eq!(v["doc"], "wavetable");
        t.join().unwrap();
        assert!(call(inst, "x", "get", Value::Null, Duration::from_millis(100)).is_err(), "nobody listening = an error, not a hang");
    }
}

// ---------------------------------------------------------------------------
// The `preset` document of every preset synth.
//
// A Preset *parameter* that recalled the other parameters could not pass
// CLAP's parameter independence (clap-validator 0.4: the values read back
// after flush() and after process() differ whenever the recall ran on one
// path and not the other). So the parameter is a label — it reads the active
// index, a host may automate it and nothing happens — and recall is an
// explicit action: the GUI's preset list, or this document from an agent:
// `plugin_set(track, "Pad", "preset", {"name": "Warm"})`.
// ---------------------------------------------------------------------------

/// The `list` entry for the `preset` document.
///
/// Plugin side (the `gui` feature, which is what links the sdk): the bus
/// tools build this crate without it and must not pull the sdk in.
#[cfg(feature = "gui")]
pub fn preset_list_entry() -> serde_json::Value {
    serde_json::json!({
        "doc": "preset",
        "get": "index, name, names[] (every factory preset)",
        "set": "{index} or {name} (case-insensitive; a unique substring will do) — recalls every parameter of that preset",
    })
}

/// Answer `get` / `set` on the `preset` document. `apply` runs the plugin's
/// own recall for a valid index (it writes the params, marks them dirty and
/// records the index as active). Plugin side, like `preset_list_entry`.
#[cfg(feature = "gui")]
pub fn preset_request(
    op: &str,
    value: &serde_json::Value,
    names: &[&str],
    active: u32,
    apply: impl FnOnce(usize),
) -> Result<serde_json::Value, String> {
    match op {
        "get" => {
            let i = (active as usize).min(names.len().saturating_sub(1));
            Ok(serde_json::json!({ "index": i, "name": names.get(i).copied().unwrap_or(""), "names": names }))
        }
        "set" => {
            let key = if let Some(i) = value.get("index").and_then(|v| v.as_u64()) {
                i.to_string()
            } else if let Some(want) = value.get("name").and_then(|v| v.as_str()) {
                want.to_string()
            } else {
                return Err("set needs {index} or {name}".into());
            };
            let idx = superduper_dsp_sdk::clap_helpers::preset_index(names, &key)?;
            apply(idx);
            Ok(serde_json::json!({ "index": idx, "name": names[idx] }))
        }
        _ => Err(format!("preset: no op {op:?} (get, set)")),
    }
}

#[cfg(all(test, feature = "gui"))]
mod preset_doc_tests {
    use super::*;

    #[test]
    fn preset_set_by_name_index_and_errors() {
        let names = ["Init", "Warm Pad", "Warm Lead"];
        let mut got = None;
        let r = preset_request("set", &serde_json::json!({"name": "warm pad"}), &names, 0, |i| got = Some(i)).unwrap();
        assert_eq!((got, r["index"].as_u64()), (Some(1), Some(1)));
        let e = preset_request("set", &serde_json::json!({"name": "warm"}), &names, 0, |_| panic!("ambiguous must not apply")).unwrap_err();
        assert!(e.contains("matches 2"), "{e}");
        let e = preset_request("set", &serde_json::json!({"index": 3}), &names, 0, |_| panic!()).unwrap_err();
        assert!(e.contains("index 3"), "{e}");
        let g = preset_request("get", &serde_json::json!({}), &names, 2, |_| panic!()).unwrap();
        assert_eq!(g["name"], "Warm Lead");
        assert_eq!(g["names"].as_array().unwrap().len(), 3);
    }
}
