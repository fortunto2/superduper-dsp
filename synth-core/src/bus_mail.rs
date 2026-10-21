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
    let tmp = path.with_extension("tmp");
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
    let _ = std::fs::remove_file(res_path(instance));
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
            let _ = std::fs::remove_file(req_path(instance));
            return Err("no answer: the plugin has no mailbox, or its host is not running".into());
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
