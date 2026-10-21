//! `sdsp-bus setup` on a throwaway home: only installed clients are touched,
//! the user's own entries survive, a second run changes nothing, and
//! `--uninstall` takes back exactly what setup added.
use sdsp_bus::setup::{Setup, SERVER};
use std::fs;
use std::path::{Path, PathBuf};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sdsp-setup-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn setup(home: &Path, skills: Option<PathBuf>, uninstall: bool) -> Vec<String> {
    Setup { home: home.into(), bin: "/opt/sd/bin/sdsp-bus".into(), skills, uninstall, dry_run: false }.run()
}

fn json(p: PathBuf) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn json_clients_merge_and_uninstall() {
    let home = tmp("json");
    fs::create_dir_all(home.join(".cursor")).unwrap();
    fs::write(home.join(".cursor/mcp.json"), r#"{"mcpServers":{"other":{"command":"x"}},"keep":1}"#).unwrap();
    fs::create_dir_all(home.join(".claude")).unwrap(); // Claude Code installed, no ~/.claude.json yet

    let out = setup(&home, None, false);
    assert!(out.iter().any(|l| l.starts_with("Claude Desktop: not installed")), "{out:?}");
    assert!(!home.join("Library").exists(), "a client that is not installed must not get a config");

    let cur = json(home.join(".cursor/mcp.json"));
    assert_eq!(cur["mcpServers"][SERVER]["command"], "/opt/sd/bin/sdsp-bus");
    assert_eq!(cur["mcpServers"][SERVER]["args"], serde_json::json!(["serve", "--stdio"]));
    assert_eq!(cur["mcpServers"]["other"]["command"], "x");
    assert_eq!(cur["keep"], 1);
    assert!(home.join(".cursor/mcp.json.sdsp-bak").exists());
    assert_eq!(json(home.join(".claude.json"))["mcpServers"][SERVER]["args"][0], "serve");

    let again = setup(&home, None, false);
    assert!(again.iter().any(|l| l == "Cursor: already set"), "{again:?}");

    setup(&home, None, true);
    let cur = json(home.join(".cursor/mcp.json"));
    assert!(cur["mcpServers"].get(SERVER).is_none());
    assert_eq!(cur["mcpServers"]["other"]["command"], "x");
}

#[test]
fn broken_json_is_left_alone() {
    let home = tmp("broken");
    fs::create_dir_all(home.join(".cursor")).unwrap();
    fs::write(home.join(".cursor/mcp.json"), "{ not json").unwrap();
    let out = setup(&home, None, false);
    assert!(out.iter().any(|l| l.contains("not valid JSON")), "{out:?}");
    assert_eq!(fs::read_to_string(home.join(".cursor/mcp.json")).unwrap(), "{ not json");
}

#[test]
fn codex_toml_block() {
    let home = tmp("codex");
    fs::create_dir_all(home.join(".codex")).unwrap();
    let before = "model = \"o3\"\n\n[mcp_servers.other]\ncommand = \"x\"\n";
    fs::write(home.join(".codex/config.toml"), before).unwrap();
    setup(&home, None, false);
    let t = fs::read_to_string(home.join(".codex/config.toml")).unwrap();
    assert!(t.starts_with(before), "{t}");
    assert!(t.contains("[mcp_servers.superduper-bus]\ncommand = \"/opt/sd/bin/sdsp-bus\"\nargs = [\"serve\", \"--stdio\"]\n"), "{t}");
    assert!(setup(&home, None, false).iter().any(|l| l == "Codex: already set"));
    setup(&home, None, true);
    assert_eq!(fs::read_to_string(home.join(".codex/config.toml")).unwrap().trim_end(), before.trim_end());
}

#[test]
fn skills_link_but_never_replace_yours() {
    let home = tmp("skills");
    let src = home.join("share/skills");
    for s in ["sdsp-mix", "sdsp-chain"] {
        fs::create_dir_all(src.join(s)).unwrap();
        fs::write(src.join(s).join("SKILL.md"), "---\nname: x\n---\n").unwrap();
    }
    fs::create_dir_all(home.join(".claude/skills/sdsp-mix")).unwrap(); // the user's own copy
    let out = setup(&home, Some(src.clone()), false);
    assert!(out.iter().any(|l| l == "Claude Code skills: linked sdsp-chain (yours, left alone: sdsp-mix)"), "{out:?}");
    assert!(out.iter().any(|l| l == "Codex skills: not installed, skipped"), "{out:?}");
    assert_eq!(fs::read_link(home.join(".claude/skills/sdsp-chain")).unwrap(), src.join("sdsp-chain"));
    setup(&home, Some(src.clone()), true);
    assert!(home.join(".claude/skills/sdsp-mix").is_dir());
    assert!(home.join(".claude/skills/sdsp-chain").symlink_metadata().is_err());
}
