//! The gateway as a client sees it, over stdio: protocol version, tool
//! annotations, prompts rendered from STRING arguments (what MCP sends — a
//! numeric field once rejected "-9"), resources readable, unknown ones an error.
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn stdio_session() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_sdsp-bus"))
        .args(["serve", "--stdio"])
        .env("SUPERDUPER_BUS", std::env::temp_dir().join(format!("sdsp-mcp-test-bus-{}", std::process::id())))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut id = 0;
    let mut rpc = |method: &str, params: Value| -> Value {
        id += 1;
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        let mut line = String::new();
        out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let init = rpc("initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}));
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    for cap in ["tools", "prompts", "resources"] {
        assert!(init["result"]["capabilities"].get(cap).is_some(), "{cap}");
    }

    let tools = rpc("tools/list", json!({}));
    let tools = tools["result"]["tools"].as_array().unwrap();
    for t in tools {
        let ro = t["annotations"]["readOnlyHint"].as_bool().unwrap_or_else(|| panic!("{} has no readOnlyHint", t["name"]));
        let writes = ["plugin_set", "plugin_set_params", "duck_under", "set_song_key"].contains(&t["name"].as_str().unwrap());
        assert_eq!(ro, !writes, "{}", t["name"]);
        assert!(t.get("outputSchema").is_some(), "{}", t["name"]);
    }

    let prompts = rpc("prompts/list", json!({}));
    assert_eq!(prompts["result"]["prompts"].as_array().unwrap().len(), 5);
    for (name, args) in [("mix-check", json!({})), ("make-room-for-vocal", json!({"vocal": "Vox"})),
                         ("duck-under", json!({"track": "Gtr", "key": "Vox"})), ("fix-mud", json!({"label": "x"})),
                         ("master-to-target", json!({"lufs": "-9"}))] {
        let r = rpc("prompts/get", json!({"name": name, "arguments": args}));
        let text = r["result"]["messages"][0]["content"]["text"].as_str().unwrap_or_else(|| panic!("{name}: {r}"));
        assert!(text.contains("superduper-bus"), "{name}");
    }
    let m = rpc("prompts/get", json!({"name": "master-to-target", "arguments": {"lufs": "-9"}}));
    assert!(m["result"]["messages"][0]["content"]["text"].as_str().unwrap().starts_with("Master to -9 LUFS"));

    let res = rpc("resources/list", json!({}));
    for r in res["result"]["resources"].as_array().unwrap() {
        let uri = r["uri"].as_str().unwrap();
        let read = rpc("resources/read", json!({"uri": uri}));
        assert!(read["result"]["contents"][0]["text"].as_str().is_some_and(|t| !t.is_empty()) || read.get("error").is_some(), "{uri}: {read}");
        if uri.starts_with("sdsp://method/") {
            assert!(read["result"]["contents"][0]["text"].as_str().unwrap().len() > 500, "{uri}");
        }
    }
    assert!(rpc("resources/read", json!({"uri": "sdsp://nope"})).get("error").is_some());
    let _ = child.kill();
}
