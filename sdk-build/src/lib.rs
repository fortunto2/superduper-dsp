//! Shared `build.rs` helpers for every SuperDuper DSP plugin.
//!
//! Usage — every plugin crate puts a one-line build.rs:
//!
//! ```ignore
//! fn main() { superduper_dsp_sdk_build::emit_build_meta(); }
//! ```
//!
//! and a `[build-dependencies]` entry on this crate.
//!
//! What it emits (as `cargo:rustc-env=` lines):
//!   - `SDSP_BUILD_NUM`   — last 5 digits of unix seconds at build time
//!   - `SDSP_BUILD_DATE`  — `YYYY-MM-DD` at build time
//!   - `SDSP_BUILD_UNIX`  — full unix seconds (for anyone who wants raw)
//!
//! Then in main code, use `superduper_dsp_sdk::build_meta` to read them at
//! compile-time and stamp the plugin display name / version string.

use std::time::{SystemTime, UNIX_EPOCH};

/// Call from a plugin's `build.rs::main()`.
pub fn emit_build_meta() {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let short = secs % 100_000;
    let (y, m, d) = days_to_ymd((secs / 86_400) as i64);

    println!("cargo:rustc-env=SDSP_BUILD_NUM={short}");
    println!("cargo:rustc-env=SDSP_BUILD_DATE={y:04}-{m:02}-{d:02}");
    println!("cargo:rustc-env=SDSP_BUILD_UNIX={secs}");

    // Force re-run on every build of the parent crate (otherwise Cargo would
    // cache the env vars). We touch a generated marker so cargo treats it as
    // perpetually outdated.
    println!("cargo:rerun-if-changed=NULL");
}

// Howard Hinnant's civil_from_days, public-domain, integer-only.
fn days_to_ymd(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { (mp + 3) as u32 } else { (mp - 9) as u32 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
