//! Read-side of the build-meta system.
//!
//! Plugins that include `superduper-dsp-sdk-build` in their `[build-dependencies]`
//! and call `emit_build_meta()` from `build.rs` will have these env vars set
//! at compile time:
//!
//!   - `SDSP_BUILD_NUM`   — short rolling integer (last 5 digits of unix seconds)
//!   - `SDSP_BUILD_DATE`  — `YYYY-MM-DD` at build time
//!   - `SDSP_BUILD_UNIX`  — full unix seconds
//!
//! Use the `plugin_display_name!` macro to assemble a versioned plugin name.

/// Compile-time build number (e.g. `"32147"`).
#[macro_export]
macro_rules! build_num {
    () => {
        env!("SDSP_BUILD_NUM")
    };
}

/// Compile-time build date (e.g. `"2026-05-17"`).
#[macro_export]
macro_rules! build_date {
    () => {
        env!("SDSP_BUILD_DATE")
    };
}

/// Compile-time version string of the form `"0.X.<BUILD_NUM> (<DATE>)"`.
/// Pass the leading SemVer prefix (e.g. `"0.2"`).
#[macro_export]
macro_rules! version_string {
    ($prefix:literal) => {
        concat!(
            $prefix,
            ".",
            env!("SDSP_BUILD_NUM"),
            " (",
            env!("SDSP_BUILD_DATE"),
            ")"
        )
    };
}

/// Compile-time plugin display name with a `[b NNNNN]` suffix so the FX
/// browser shows which build is installed without sacrificing the CLAP id
/// (which stays stable, preserving REAPER track-level caches).
#[macro_export]
macro_rules! plugin_display_name {
    ($base:literal) => {
        concat!($base, " [b", env!("SDSP_BUILD_NUM"), "]")
    };
}
