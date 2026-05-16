//! MCP server (HTTP/SSE) for Claude Code.
//!
//! Implements the Model Context Protocol over Server-Sent Events transport.
//! Exposes tools listed in SPEC.md F4.

use crate::registry::SharedRegistry;
use anyhow::Result;
use axum::{routing::get, Router};

/// Run MCP server on the given port.
pub async fn run(port: u16, _registry: SharedRegistry) -> Result<()> {
    let app = Router::new()
        .route("/", get(|| async { "SuperDuper DSP MCP server" }))
        .route("/sse", get(sse_handler));
    // TODO M2: full MCP transport. Recommended approach:
    //   - GET /sse        → opens SSE stream
    //   - POST /message   → client → server messages
    //   - JSON-RPC 2.0 framing
    //   - Tool definitions per SPEC.md F4

    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("MCP server listening on http://{addr}/sse");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn sse_handler() -> &'static str {
    // TODO M2: real SSE stream with MCP framing
    "MCP SSE endpoint stub"
}
