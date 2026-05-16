//! Effect compilation pipeline.
//!
//! M3 scope.
//!
//! Layout of an instance's working directory: ~/.superduper-dsp/instances/<uuid>/
//!   Cargo.toml         (auto-generated; depends on superduper-dsp-sdk)
//!   src/lib.rs         (contains the user's process.rs renamed as lib.rs)
//!   target/            (cargo's incremental cache)
//!
//! On load_effect:
//!   1. Write incoming code to src/lib.rs
//!   2. Spawn `cargo build --release --crate-type cdylib` with `current_dir`
//!   3. Capture stdout+stderr
//!   4. On success: locate target/release/libinstance_<uuid>.dylib
//!   5. Inspect dylib for params (see dylib_inspector.rs)
//!   6. Return path to plugin via IPC LoadDylib message

use anyhow::Result;
use std::path::PathBuf;
use uuid::Uuid;

#[allow(dead_code)]
pub struct BuildResult {
    pub success: bool,
    pub dylib_path: Option<PathBuf>,
    pub log: String,
}

#[allow(dead_code)]
pub async fn build_effect(instance_id: Uuid, code: &str) -> Result<BuildResult> {
    let _ = (instance_id, code);
    // TODO M3
    Ok(BuildResult {
        success: false,
        dylib_path: None,
        log: "build_pipeline not yet implemented".to_string(),
    })
}
