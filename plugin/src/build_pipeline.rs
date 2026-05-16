//! `cargo build` pipeline for AI-authored DSP effects.
//!
//! MCP `load_effect(code)` lands here:
//!
//! 1. Pick a fresh build directory under `~/.superduper-dsp/effect-builds/<uuid>/`.
//!    We do NOT compile inside the instance dir — Cargo creates a `target/`
//!    full of intermediate files and the watcher would flap on every linker
//!    write.
//! 2. Generate a minimal `Cargo.toml` + write the user's source into
//!    `src/lib.rs`. The crate depends on `superduper-dsp-sdk` by absolute path
//!    so effects pick up the same `ParamMeta` ABI as the host plugin.
//! 3. Run `cargo build --release` with the host's own target triple. Capture
//!    stdout+stderr for the response so Claude can iterate on compile errors.
//! 4. Copy the resulting `lib*.dylib` into the primary instance's
//!    `effect.dylib` path — the watcher already listening there picks it up
//!    and `HotReloadSlot::swap` runs, the way every other M1 swap does.
//!
//! Non-RT thread only. Tokio current-thread runtime inside the MCP server
//! drives this; the audio thread never touches any of it.

use crate::dbg_log;
use std::path::{Path, PathBuf};

macro_rules! dlog { ($($arg:tt)*) => { dbg_log(format_args!($($arg)*)) } }

/// Where the SDK crate lives on this developer's machine. Used to point the
/// generated Cargo.toml at the right `superduper-dsp-sdk = { path = ... }`.
///
/// Resolution order:
/// 1. `SDSP_SDK_DIR` env var if set.
/// 2. The directory recorded at compile time by `env!("CARGO_MANIFEST_DIR")`
///    on the plugin crate, walking up to the workspace root + `sdk/`.
fn sdk_path() -> PathBuf {
    if let Some(env) = std::env::var_os("SDSP_SDK_DIR") {
        return PathBuf::from(env);
    }
    // CARGO_MANIFEST_DIR at compile time = .../superduper-dsp/plugin
    let plugin_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    plugin_dir
        .parent()
        .map(|workspace| workspace.join("sdk"))
        .unwrap_or_else(|| PathBuf::from("./sdk"))
}

/// Output of a build attempt.
pub struct BuildOutput {
    pub success: bool,
    pub log: String,
    /// Absolute path to the built dylib if `success`.
    pub dylib: Option<PathBuf>,
}

/// Build an effect dylib from raw Rust source. Synchronous (blocks on cargo).
///
/// `name` is a short identifier used to pick a stable build subdirectory.
/// Re-using the same name across calls keeps the Cargo cache warm so
/// subsequent builds are ~10x faster than a cold one.
pub fn build(name: &str, code: &str) -> BuildOutput {
    let root = match dirs::home_dir() {
        Some(h) => h
            .join(".superduper-dsp")
            .join("effect-builds")
            .join(sanitize(name)),
        None => PathBuf::from("/tmp/superduper-dsp-build"),
    };

    if let Err(e) = std::fs::create_dir_all(root.join("src")) {
        return BuildOutput {
            success: false,
            log: format!("could not create build dir {:?}: {}", root, e),
            dylib: None,
        };
    }

    let crate_name = format!("effect_{}", sanitize(name));
    let sdk = sdk_path();
    let cargo_toml = generate_cargo_toml(&crate_name, &sdk);
    if let Err(e) = std::fs::write(root.join("Cargo.toml"), cargo_toml) {
        return BuildOutput {
            success: false,
            log: format!("write Cargo.toml: {}", e),
            dylib: None,
        };
    }
    if let Err(e) = std::fs::write(root.join("src/lib.rs"), code) {
        return BuildOutput {
            success: false,
            log: format!("write src/lib.rs: {}", e),
            dylib: None,
        };
    }

    dlog!("build_pipeline: cargo build in {:?}", root);

    // Force host-arch target explicitly so REAPER-under-Rosetta scenarios
    // still get a dylib that dlopen will accept.
    let target_triple = host_target();

    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("build")
        .arg("--release")
        .arg("--target")
        .arg(&target_triple)
        .current_dir(&root)
        // Detach from any inherited target dir (parent plugin's) so we don't
        // race with the workspace's own builds.
        .env_remove("CARGO_TARGET_DIR")
        .env(
            "CARGO_TARGET_DIR",
            root.join("target").as_os_str().to_owned(),
        );

    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            return BuildOutput {
                success: false,
                log: format!("cargo invocation failed: {}", e),
                dylib: None,
            };
        }
    };

    let mut log = String::new();
    log.push_str(&String::from_utf8_lossy(&output.stdout));
    log.push_str(&String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        return BuildOutput {
            success: false,
            log,
            dylib: None,
        };
    }

    let dylib_name = format!("lib{}.dylib", crate_name);
    let dylib = root
        .join("target")
        .join(&target_triple)
        .join("release")
        .join(&dylib_name);

    if !dylib.exists() {
        return BuildOutput {
            success: false,
            log: format!(
                "{}\nbuild succeeded but {:?} was not produced",
                log, dylib
            ),
            dylib: None,
        };
    }

    BuildOutput {
        success: true,
        log,
        dylib: Some(dylib),
    }
}

/// Copy a built dylib into the live instance directory so the watcher /
/// Reload toggle picks it up.
pub fn install_dylib(built: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::copy(built, dest)?;
    // Bump mtime so notify reliably fires Modify even when the bytes match.
    let _ = std::fs::File::open(dest).and_then(|f| f.sync_all());
    Ok(())
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn host_target() -> String {
    // Detect at compile time. Plugin and effect always end up the same arch
    // (a Rosetta-launched REAPER hosts an x86_64 plugin, and that plugin
    // produces x86_64 effects via this fn, so they match).
    if cfg!(target_arch = "aarch64") {
        "aarch64-apple-darwin".into()
    } else {
        "x86_64-apple-darwin".into()
    }
}

fn generate_cargo_toml(crate_name: &str, sdk_path: &Path) -> String {
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
superduper-dsp-sdk = {{ path = "{sdk}" }}

[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
panic = "unwind"
strip = false
"#,
        name = crate_name,
        sdk = sdk_path.display(),
    )
}
