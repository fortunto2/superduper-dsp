//! Shared types for SuperDuper DSP IPC and MCP API.
//!
//! - Plugin ↔ Daemon: JSON-lines over Unix domain socket.
//! - Claude Code ↔ Daemon: JSON-RPC over HTTP/SSE (separate transport).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Default Unix socket path.
pub const SOCKET_PATH_ENV: &str = "SUPERDUPER_DSP_SOCKET";
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/superduper-dsp.sock";

/// Default MCP HTTP port.
pub const DEFAULT_MCP_PORT: u16 = 7891;

/// Protocol version. Bumped on breaking IPC changes.
pub const PROTOCOL_VERSION: u32 = 1;

// ============================================================================
// Plugin → Daemon
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginToDaemon {
    /// First message on connect.
    Register {
        instance_id: Uuid,
        track_name: Option<String>,
        protocol_version: u32,
    },
    /// Clean shutdown.
    Unregister { instance_id: Uuid },
    /// Liveness ping every 5 sec.
    Heartbeat { instance_id: Uuid },
    /// Plugin loaded a new dylib successfully.
    EffectLoaded {
        instance_id: Uuid,
        params: Vec<ParamInfo>,
    },
    /// Runtime error (e.g. panic in process).
    Error {
        instance_id: Uuid,
        message: String,
    },
}

// ============================================================================
// Daemon → Plugin
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DaemonToPlugin {
    /// Registration accepted.
    RegisterAck { instance_id: Uuid },
    /// Registration refused (e.g. version mismatch).
    RegisterRefused { reason: String },
    /// Load a freshly compiled dylib at this path.
    LoadDylib {
        path: String,
        params: Vec<ParamInfo>,
    },
    /// Set parameter (lock-free safe in audio thread).
    SetParam { name: String, value: f32 },
    /// Bypass toggle.
    SetBypass { enabled: bool },
    /// Daemon shutting down, please disconnect.
    Shutdown,
}

// ============================================================================
// Domain types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ParamInfo {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub current: f32,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceSummary {
    pub id: Uuid,
    pub name: Option<String>,
    pub track_name: Option<String>,
    pub current_effect: Option<String>,
    pub status: InstanceStatus,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstanceStatus {
    Idle,
    Compiling,
    Running,
    Error,
}

// ============================================================================
// MCP tool requests / responses
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadEffectRequest {
    /// Target instance: UUID, instance name, or track name.
    pub target: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadEffectResponse {
    pub success: bool,
    pub compile_log: String,
    pub params: Option<Vec<ParamInfo>>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetParamRequest {
    pub target: String,
    pub name: String,
    pub value: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenericResponse {
    pub success: bool,
    pub error: Option<String>,
}
