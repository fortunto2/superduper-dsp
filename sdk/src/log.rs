//! Shared plugin logging — write a tagged log line to
//! `~/.superduper-dsp/<plugin>.log`. Before this module every plugin
//! defined its own `log_path` / `init_logging` / `slog_args` / `slog!`
//! macro (~35 lines, all identical). Now: one `init("name")` call at
//! activate-time, then `slog!` anywhere.
//!
//! Logging is NOT real-time safe — it holds a `parking_lot::Mutex`
//! and calls `writeln!` which can block on disk. Don't call `slog!`
//! from inside the audio thread. The existing call sites all sit in
//! GUI / event-handler paths, which is fine.

use parking_lot::Mutex;
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::OnceLock;

static LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();
static PLUGIN_NAME: OnceLock<String> = OnceLock::new();

fn log_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".superduper-dsp")
}

/// Open / create the per-plugin log file. Idempotent — subsequent
/// calls after the first are no-ops, so calling from every
/// `activate()` is safe (and the first call wins the plugin name).
pub fn init(plugin_name: &'static str) {
    LOG_FILE.get_or_init(|| {
        let _ = PLUGIN_NAME.set(plugin_name.to_string());
        let dir = log_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.log", plugin_name));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        Mutex::new(file)
    });
}

/// Append one log line. Format is `[ms-since-epoch] message`. Use the
/// `slog!` macro from `superduper_dsp_sdk` so call sites stay tidy.
pub fn log_args(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    if let Some(slot) = LOG_FILE.get() {
        if let Some(file) = slot.lock().as_mut() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let _ = writeln!(file, "[{}] {}", now, args);
        }
    }
}

/// Append one log line through `format_args!`. Use as `slog!("hit {}", x)`.
/// Re-exported at the crate root so plugins can `use superduper_dsp_sdk::slog;`.
#[macro_export]
macro_rules! slog {
    ($($arg:tt)*) => {
        $crate::log::log_args(format_args!($($arg)*))
    };
}
