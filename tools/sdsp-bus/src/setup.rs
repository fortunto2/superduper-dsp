//! `sdsp-bus setup` — register the bus gateway as an MCP server in every agent
//! client this user has, and link the skills that teach them to mix with it.
//!
//! Run by the macOS installer's postinstall (as the console user) and by hand.
//! Only touches a client whose config directory already exists, only its own
//! `superduper-bus` entry, and never a skill folder it did not create;
//! `--uninstall` takes back exactly that.

use std::fs;
use std::path::{Path, PathBuf};

pub const SERVER: &str = "superduper-bus";

pub struct Setup {
    pub home: PathBuf,
    /// The gateway binary the clients will start.
    pub bin: PathBuf,
    /// Folder of skill folders (`<name>/SKILL.md`); None = skip skills.
    pub skills: Option<PathBuf>,
    pub uninstall: bool,
    pub dry_run: bool,
}

/// Where each JSON client keeps its servers, relative to home.
/// (name, config file, the directory that proves the client is installed)
const JSON_CLIENTS: &[(&str, &str, &str)] = &[
    ("Claude Code", ".claude.json", ".claude"),
    ("Claude Desktop", "Library/Application Support/Claude/claude_desktop_config.json", "Library/Application Support/Claude"),
    ("Cursor", ".cursor/mcp.json", ".cursor"),
    ("Windsurf", ".codeium/windsurf/mcp_config.json", ".codeium/windsurf"),
];

impl Setup {
    pub fn run(&self) -> Vec<String> {
        let mut out = Vec::new();
        for (name, file, dir) in JSON_CLIENTS {
            out.push(format!("{name}: {}", self.json_client(&self.home.join(file), &self.home.join(dir))));
        }
        out.push(format!("Codex: {}", self.codex()));
        if let Some(src) = &self.skills {
            for (name, root) in [("Claude Code skills", ".claude/skills"), ("Codex skills", ".codex/skills")] {
                out.push(format!("{name}: {}", self.link_skills(src, root)));
            }
        }
        out
    }

    fn entry(&self) -> serde_json::Value {
        serde_json::json!({ "command": self.bin.to_string_lossy(), "args": ["serve", "--stdio"] })
    }

    fn json_client(&self, file: &Path, installed: &Path) -> String {
        if !installed.is_dir() {
            return "not installed, skipped".into();
        }
        let mut doc: serde_json::Value = match fs::read_to_string(file) {
            Ok(s) if !s.trim().is_empty() => match serde_json::from_str(&s) {
                Ok(v) => v,
                Err(e) => return format!("{} is not valid JSON ({e}), left alone", file.display()),
            },
            _ => serde_json::json!({}),
        };
        let Some(obj) = doc.as_object_mut() else {
            return format!("{} is not a JSON object, left alone", file.display());
        };
        let servers = obj.entry("mcpServers").or_insert_with(|| serde_json::json!({}));
        let Some(servers) = servers.as_object_mut() else {
            return "mcpServers is not an object, left alone".into();
        };
        let changed = if self.uninstall {
            servers.remove(SERVER).is_some()
        } else if servers.get(SERVER) == Some(&self.entry()) {
            false
        } else {
            servers.insert(SERVER.into(), self.entry());
            true
        };
        if !changed {
            return if self.uninstall { "nothing to remove".into() } else { "already set".into() };
        }
        if !self.dry_run {
            if let Err(e) = write_with_backup(file, &(serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n")) {
                return format!("write failed: {e}");
            }
        }
        format!("{} {}", if self.uninstall { "removed from" } else { "added to" }, file.display())
    }

    fn codex(&self) -> String {
        let dir = self.home.join(".codex");
        if !dir.is_dir() {
            return "not installed, skipped".into();
        }
        let file = dir.join("config.toml");
        let text = fs::read_to_string(&file).unwrap_or_default();
        let header = format!("[mcp_servers.{SERVER}]");
        let stripped = strip_toml_table(&text, &header);
        let had = stripped != text;
        let new = if self.uninstall {
            if !had {
                return "nothing to remove".into();
            }
            stripped
        } else {
            let block = format!(
                "{header}\ncommand = {}\nargs = [\"serve\", \"--stdio\"]\n",
                toml_str(&self.bin.to_string_lossy())
            );
            let sep = if stripped.is_empty() || stripped.ends_with("\n\n") { "" } else if stripped.ends_with('\n') { "\n" } else { "\n\n" };
            let new = format!("{stripped}{sep}{block}");
            if new == text {
                return "already set".into();
            }
            new
        };
        if !self.dry_run {
            if let Err(e) = write_with_backup(&file, &new) {
                return format!("write failed: {e}");
            }
        }
        format!("{} {}", if self.uninstall { "removed from" } else if had { "updated in" } else { "added to" }, file.display())
    }

    fn link_skills(&self, src: &Path, root: &str) -> String {
        let client = self.home.join(root.split('/').next().unwrap_or(root));
        if !client.is_dir() {
            return "not installed, skipped".into();
        }
        let dest = self.home.join(root);
        let Ok(entries) = fs::read_dir(src) else {
            return format!("no skills at {}", src.display());
        };
        let mut names: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.join("SKILL.md").is_file()).collect();
        names.sort();
        let (mut done, mut kept) = (Vec::new(), Vec::new());
        for skill in names {
            let Some(name) = skill.file_name() else { continue };
            let link = dest.join(name);
            let ours = fs::read_link(&link).map(|t| t == skill).unwrap_or(false);
            let name = name.to_string_lossy().into_owned();
            if self.uninstall {
                if ours && (self.dry_run || fs::remove_file(&link).is_ok()) {
                    done.push(name);
                }
            } else if ours {
                continue;
            } else if link.symlink_metadata().is_ok() {
                kept.push(name);
            } else if self.dry_run || (fs::create_dir_all(&dest).is_ok() && symlink(&skill, &link).is_ok()) {
                done.push(name);
            }
        }
        let mut msg = if done.is_empty() {
            if self.uninstall { "nothing to remove".to_string() } else { "already linked".to_string() }
        } else {
            format!("{} {}", if self.uninstall { "unlinked" } else { "linked" }, done.join(", "))
        };
        if !kept.is_empty() {
            msg += &format!(" (yours, left alone: {})", kept.join(", "));
        }
        msg
    }
}

/// Drop `header` and its keys up to the next table header.
fn strip_toml_table(text: &str, header: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in text.split_inclusive('\n') {
        let t = line.trim();
        if t == header {
            skipping = true;
            continue;
        }
        if skipping && t.starts_with('[') {
            skipping = false;
        }
        if !skipping {
            out.push_str(line);
        }
    }
    out
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Keep the user's original once as `<file>.sdsp-bak`, then replace atomically.
fn write_with_backup(file: &Path, text: &str) -> std::io::Result<()> {
    let bak = file.with_extension(format!("{}sdsp-bak", file.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()));
    if file.exists() && !bak.exists() {
        fs::copy(file, &bak)?;
    }
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("sdsp-tmp");
    fs::write(&tmp, text)?;
    fs::rename(tmp, file)
}

#[cfg(unix)]
fn symlink(src: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, link)
}
#[cfg(windows)]
fn symlink(src: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(src, link)
}

/// Default skills folder next to the binary: `<prefix>/bin/sdsp-bus` → `<prefix>/skills`.
pub fn default_skills(bin: &Path) -> Option<PathBuf> {
    let p = bin.parent()?.parent()?.join("skills");
    p.is_dir().then_some(p)
}

pub fn main(args: &[String]) -> i32 {
    let flag = |f: &str| args.iter().any(|a| a == f);
    let opt = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).map(PathBuf::from);
    if flag("--help") || flag("-h") {
        println!("sdsp-bus setup [--uninstall] [--dry-run] [--home DIR] [--bin PATH] [--skills DIR | --no-skills]\n\
                  Registers `{SERVER}` (this binary, `serve --stdio`) with Claude Code, Claude Desktop,\n\
                  Cursor, Windsurf and Codex, and links the skills into ~/.claude/skills and ~/.codex/skills.\n\
                  Clients that are not installed are skipped; nothing of yours is overwritten.");
        return 0;
    }
    let Some(home) = opt("--home").or_else(|| std::env::var_os("HOME").map(PathBuf::from)) else {
        eprintln!("setup: no HOME");
        return 1;
    };
    let bin = opt("--bin").or_else(|| std::env::current_exe().ok().and_then(|p| p.canonicalize().ok())).unwrap_or_else(|| PathBuf::from("sdsp-bus"));
    let skills = if flag("--no-skills") { None } else { opt("--skills").or_else(|| default_skills(&bin)) };
    let s = Setup { home, bin, skills, uninstall: flag("--uninstall"), dry_run: flag("--dry-run") };
    for line in s.run() {
        println!("{line}");
    }
    if !s.uninstall {
        println!("\nRestart the agent app to pick up `{SERVER}`. REAPER control is a separate MCP server\n\
                  (total-reaper-mcp) — the superduper-song skill walks through it.");
    }
    0
}
