//! Save and load sessions.
//!
//! M6 scope.
//!
//! Session file format (~/.superduper-dsp/sessions/<name>.toml):
//!
//! ```toml
//! version = 1
//! created_at = "2026-05-16T..."
//!
//! [[instance]]
//! name = "Lead"
//! track_name = "Lead"
//! code = """
//! use superduper_dsp_sdk::*;
//! setup!();
//! # ... user code
//! """
//!
//! [instance.params]
//! DRIVE = 0.5
//! TONE = 0.0
//! ```

use anyhow::Result;
use std::path::PathBuf;

#[allow(dead_code)]
pub fn sessions_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".superduper-dsp").join("sessions")
}

#[allow(dead_code)]
pub fn save_session(_name: &str) -> Result<PathBuf> {
    // TODO M6
    Ok(sessions_dir().join("placeholder.toml"))
}

#[allow(dead_code)]
pub fn load_session(_name: &str) -> Result<usize> {
    // TODO M6 — returns number of instances restored
    Ok(0)
}
