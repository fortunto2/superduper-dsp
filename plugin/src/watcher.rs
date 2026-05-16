//! Background file-watcher that drives [`HotReloadSlot::swap`].
//!
//! On every Modify/Create event for the per-instance dylib path, the watcher
//! thread calls `slot.swap()` and runs `slot.gc()` periodically to drop
//! grace-period-expired libraries.
//!
//! Lives for the lifetime of [`crate::PluginShared`] via a RAII handle.

use crate::{HotReloadSlot, dbg_log};
use notify::{EventKind, RecursiveMode, Watcher};

macro_rules! dlog { ($($arg:tt)*) => { dbg_log(format_args!($($arg)*)) } }
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// RAII handle: dropping it signals the watcher thread to exit and joins it.
pub struct WatcherHandle {
    shutdown_tx: Sender<()>,
    join: Option<JoinHandle<()>>,
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        let _ = self.shutdown_tx.send(());
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Start the watcher. Returns `Err` if the OS-level watcher could not be
/// initialised (rare — `dirs` failed, no permissions, etc).
///
/// `dylib_path` is the per-instance dylib file; only events matching it
/// trigger a swap. We watch the parent directory non-recursively because
/// `notify` doesn't fire when watching a not-yet-existing file.
pub fn start(slot: Arc<HotReloadSlot>, dylib_path: PathBuf) -> notify::Result<WatcherHandle> {
    let (shutdown_tx, shutdown_rx) = channel::<()>();
    let (notify_tx, notify_rx) = channel::<notify::Event>();

    let parent = dylib_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        if let Ok(event) = result {
            let _ = notify_tx.send(event);
        }
    })?;
    watcher.watch(&parent, RecursiveMode::NonRecursive)?;

    let join = thread::Builder::new()
        .name("sdsp-watcher".into())
        .spawn(move || watcher_loop(watcher, slot, dylib_path, shutdown_rx, notify_rx))
        .expect("spawning watcher thread");

    Ok(WatcherHandle {
        shutdown_tx,
        join: Some(join),
    })
}

fn watcher_loop<W: Watcher + Send + 'static>(
    _watcher: W,
    slot: Arc<HotReloadSlot>,
    dylib_path: PathBuf,
    shutdown_rx: Receiver<()>,
    notify_rx: Receiver<notify::Event>,
) {
    // `_watcher` is owned here so the underlying OS watcher stays alive for the
    // lifetime of the thread.
    let mut last_swap = Instant::now() - Duration::from_secs(1);

    loop {
        // Tick at 100ms. Use shutdown_rx as the alarm clock so we can exit fast.
        match shutdown_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }

        // Drop libraries past their grace period.
        slot.gc();

        // Drain notify events, look for ones that touch our dylib.
        // We match on filename (not full Path equality) because macOS FSEvents
        // sometimes returns `/private/Users/...` while we hold `/Users/...` —
        // a symlink difference that breaks PathBuf equality.
        let target_name = dylib_path.file_name();
        let mut should_swap = false;
        while let Ok(event) = notify_rx.try_recv() {
            let touches = event
                .paths
                .iter()
                .any(|p| p.file_name() == target_name);
            if !touches {
                continue;
            }
            dlog!(
                "watcher event {:?} paths={:?}",
                event.kind,
                event
                    .paths
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
            );
            // Accept anything that isn't pure Access (file open/close noise).
            // Modify(_), Create(_), Other, Any — all should provoke a swap.
            match event.kind {
                EventKind::Access(_) => {}
                _ => {
                    should_swap = true;
                }
            }
        }

        // Debounce: a single `cargo build` produces several Modify events as
        // the linker writes out the dylib in chunks. Wait a tick between swaps.
        if should_swap
            && dylib_path.exists()
            && last_swap.elapsed() > Duration::from_millis(150)
        {
            match slot.swap(&dylib_path) {
                Ok(()) => {
                    tracing::info!("hot-reloaded effect from {:?}", dylib_path);
                }
                Err(e) => {
                    tracing::warn!("hot-reload of {:?} failed: {}", dylib_path, e);
                }
            }
            last_swap = Instant::now();
        }
    }
}
