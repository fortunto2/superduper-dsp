//! Unix socket server: handles plugin instance connections.
//!
//! Wire format: JSON-lines (newline-delimited JSON objects).
//! One connection = one plugin instance.

use crate::registry::SharedRegistry;
use anyhow::Result;

/// Run the IPC server until shutdown.
///
/// On startup, removes any stale socket file at `path`, then binds and accepts
/// connections. Each connection is handled in a spawned task.
pub async fn run(path: &str, _registry: SharedRegistry) -> Result<()> {
    // TODO M1:
    // 1. std::fs::remove_file(path).ok();
    // 2. let listener = interprocess::local_socket::tokio::Listener::bind(path)?;
    // 3. loop { accept; spawn handle_connection }
    //
    // handle_connection:
    //   - Read first line, parse PluginToDaemon::Register
    //   - Verify protocol_version
    //   - Send DaemonToPlugin::RegisterAck
    //   - Insert into registry
    //   - Loop:
    //       - read line → PluginToDaemon (heartbeat / unregister / effect_loaded / error)
    //       - update registry accordingly
    //   - On disconnect or unregister: remove from registry

    tracing::info!("IPC server stub on {}", path);
    std::future::pending::<()>().await;
    Ok(())
}
