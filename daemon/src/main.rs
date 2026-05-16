//! superduperd — SuperDuper DSP daemon.
//!
//! Singleton process that:
//! - Listens on a Unix domain socket for plugin instance connections
//! - Listens on HTTP/SSE for MCP clients (Claude Code)
//! - Runs the build pipeline (cargo build) when effects are loaded
//! - Holds the registry of all live plugin instances and routes commands

mod build_pipeline;
mod dylib_inspector;
mod ipc;
mod mcp;
mod registry;
mod sessions;

use superduper_dsp_protocol::{DEFAULT_MCP_PORT, DEFAULT_SOCKET_PATH};
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    info!("superduperd starting");

    let socket_path = std::env::var(superduper_dsp_protocol::SOCKET_PATH_ENV)
        .unwrap_or_else(|_| DEFAULT_SOCKET_PATH.to_string());
    let mcp_port: u16 = std::env::var("SUPERDUPER_DSP_MCP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MCP_PORT);

    info!(socket = %socket_path, mcp_port, "config loaded");

    let registry = registry::Registry::new_shared();

    // Spawn IPC server (plugin-side Unix socket)
    let ipc_handle = {
        let registry = registry.clone();
        let path = socket_path.clone();
        tokio::spawn(async move {
            if let Err(e) = ipc::run(&path, registry).await {
                tracing::error!("IPC server crashed: {e:#}");
            }
        })
    };

    // Spawn MCP server (HTTP/SSE for Claude Code)
    let mcp_handle = {
        let registry = registry.clone();
        tokio::spawn(async move {
            if let Err(e) = mcp::run(mcp_port, registry).await {
                tracing::error!("MCP server crashed: {e:#}");
            }
        })
    };

    // Wait for either to exit. In practice both run forever or until shutdown.
    tokio::select! {
        _ = ipc_handle => info!("IPC task ended"),
        _ = mcp_handle => info!("MCP task ended"),
        _ = tokio::signal::ctrl_c() => info!("SIGINT received, shutting down"),
    }

    info!("superduperd exiting");
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,superduperd=debug"));
    fmt().with_env_filter(filter).init();
}
