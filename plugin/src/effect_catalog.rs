//! On-disk catalog of compiled effects.
//!
//! Scans `~/.superduper-dsp/effect-builds/<name>/target/<triple>/release/
//! libeffect_<name>.dylib` and returns whichever effects have a built dylib.
//! Used by the `Effect ▼` CLAP enum-param so the user can browse and switch
//! between everything Claude has compiled, all from inside the REAPER FX
//! window.

use std::path::PathBuf;

/// One entry in the live catalog.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub name: String,
    pub dylib: PathBuf,
}

fn builds_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".superduper-dsp")
        .join("effect-builds")
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Fresh scan. Cheap enough (a few dozen `path.exists()` syscalls); caller
/// holds the result in an `ArcSwap` so callers don't redo it for every CLAP
/// `get_info` query.
pub fn scan() -> Vec<CatalogEntry> {
    let root = builds_dir();
    let mut entries = Vec::new();
    let Ok(read) = std::fs::read_dir(&root) else {
        return entries;
    };
    let mut dirs: Vec<_> = read.flatten().collect();
    dirs.sort_by_key(|e| e.file_name());
    for entry in dirs {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let dylib_name = format!("libeffect_{}.dylib", sanitize(&name));
        let mut found = None;
        for triple in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
            let candidate = path
                .join("target")
                .join(triple)
                .join("release")
                .join(&dylib_name);
            if candidate.exists() {
                found = Some(candidate);
                break;
            }
        }
        if let Some(dylib) = found {
            entries.push(CatalogEntry { name, dylib });
        }
    }
    entries
}
