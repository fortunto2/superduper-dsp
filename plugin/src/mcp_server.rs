//! In-plugin MCP server (Streamable HTTP) so external agents — Claude Code,
//! Codex, anyone speaking MCP — can drive this plugin instance without going
//! through a DAW UI: read the current effect state, change parameters live,
//! and (M3.4) ship freshly written Rust DSP into the slot.
//!
//! Runs on a dedicated OS thread with its own tokio current-thread runtime.
//! Listens on `127.0.0.1:0` (kernel-assigned port). The bound URL is written
//! to `~/.superduper-dsp/mcp-url.txt` so external tooling can discover it
//! without us hard-coding a port.

use crate::{PluginShared, build_pipeline, dbg_log, mcp_registry};
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    schemars,
    transport::streamable_http_server::{
        StreamableHttpService,
        session::local::LocalSessionManager,
        tower::StreamableHttpServerConfig,
    },
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;

fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Best-effort regex-free parse of a `params! { … }` block in an effect's
/// `src/lib.rs`. Recognises lines that look like
/// `NAME = param(MIN, MAX).default(D).unit("U")` (each `.default()` /
/// `.unit()` optional). Unrecognised lines are silently dropped — this is
/// only for the list_effects display, not for ABI correctness.
fn parse_params_from_source(src: &str) -> Vec<EffectParamView> {
    let Some(rest) = src.split_once("params!").map(|(_, r)| r) else {
        return Vec::new();
    };
    let Some(rest) = rest.split_once('{').map(|(_, r)| r) else {
        return Vec::new();
    };
    let Some((block, _)) = rest.split_once('}') else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for raw_line in block.lines() {
        let line = raw_line.trim().trim_end_matches(',').trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let Some((name, expr)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        // Skip patterns that obviously aren't param decls.
        if !name.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
            continue;
        }

        let Some((before_paren, after)) = expr.split_once('(') else {
            continue;
        };
        if !before_paren.trim().ends_with("param") {
            continue;
        }
        let Some((args, tail)) = after.split_once(')') else {
            continue;
        };
        let nums: Vec<&str> = args.split(',').map(str::trim).collect();
        if nums.len() != 2 {
            continue;
        }
        let (Ok(min), Ok(max)) = (nums[0].parse::<f32>(), nums[1].parse::<f32>()) else {
            continue;
        };

        // Pull .default(...) and .unit("...") off the tail if present.
        let default = extract_call_f32(tail, "default").unwrap_or((min + max) * 0.5);
        let unit = extract_call_str(tail, "unit").unwrap_or_default();

        out.push(EffectParamView {
            name: name.into(),
            min,
            max,
            default,
            unit,
        });
    }
    out
}

fn extract_call_f32(tail: &str, fn_name: &str) -> Option<f32> {
    let needle = format!(".{}(", fn_name);
    let i = tail.find(&needle)? + needle.len();
    let j = tail[i..].find(')')? + i;
    tail[i..j].trim().parse().ok()
}

fn extract_call_str(tail: &str, fn_name: &str) -> Option<String> {
    let needle = format!(".{}(", fn_name);
    let i = tail.find(&needle)? + needle.len();
    let j = tail[i..].find(')')? + i;
    let arg = tail[i..j].trim();
    let arg = arg.strip_prefix('"')?.strip_suffix('"')?;
    Some(arg.into())
}

/// Convenience: tool handlers all need `&PluginShared`. Return an error if
/// the primary instance hasn't been registered yet.
fn shared() -> Result<&'static PluginShared, McpError> {
    mcp_registry::primary().ok_or_else(|| {
        McpError::internal_error(
            "no SuperDuper DSP instance is currently primary — load the plugin in a DAW first",
            None,
        )
    })
}

macro_rules! dlog { ($($arg:tt)*) => { dbg_log(format_args!($($arg)*)) } }

// ===========================================================================
// Tool I/O schemas
// ===========================================================================

#[derive(Serialize, schemars::JsonSchema)]
pub struct StatusOutput {
    pub instance_id: String,
    pub loaded: bool,
    pub poisoned: bool,
    pub effect_dylib_path: String,
    pub gain_db: f32,
    pub bypassed: bool,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ParamView {
    pub name: String,
    pub value: f32,
    pub min: f32,
    pub max: f32,
    pub unit: String,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ParamsOutput {
    pub params: Vec<ParamView>,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct SetParamInput {
    /// Parameter name. Special: "gain" for host gain, otherwise an effect-defined
    /// param name. Case-sensitive — match what `get_params` returns.
    pub name: String,
    /// Plain value (e.g. dB for gain, 0..1 for drive). Will be clamped to
    /// the param's declared min/max.
    pub value: f32,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct SetParamOutput {
    pub ok: bool,
    pub applied_value: f32,
    pub note: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct BypassInput {
    pub enabled: bool,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct BypassOutput {
    pub bypassed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
pub struct LoadEffectInput {
    /// Short ASCII identifier used for the on-disk build cache directory and
    /// the generated crate name. Re-using the same name keeps the Cargo
    /// incremental cache warm (subsequent builds ~10x faster). Use something
    /// like "tape_saturation" or "biquad_lowpass".
    pub name: String,
    /// Full source text for the effect crate's `src/lib.rs`. Must use
    /// `superduper_dsp_sdk::*`, declare params via `params!{}`, call
    /// `setup!();`, and export a `process` fn matching the SDK contract.
    pub code: String,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct LoadEffectOutput {
    pub success: bool,
    /// Cargo's stdout + stderr. On failure this is the compile error log
    /// — pipe it back into your next attempt.
    pub log: String,
    /// Where the freshly built dylib landed (the watcher will already have
    /// swapped it into the audio thread by the time you read this).
    pub installed_at: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct EffectParamView {
    pub name: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: String,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct EffectListing {
    pub name: String,
    pub built: bool,
    /// Full `src/lib.rs` source the effect was authored from.
    pub source: String,
    /// Parameters declared via the `params!` macro. Parsed from source —
    /// works even when the dylib isn't built.
    pub params: Vec<EffectParamView>,
    pub dylib_path: Option<String>,
}

#[derive(Serialize, schemars::JsonSchema)]
pub struct ListEffectsOutput {
    pub current_effect_dylib: String,
    pub effects: Vec<EffectListing>,
}

// ===========================================================================
// Server
// ===========================================================================

#[derive(Clone)]
pub struct McpServer {
    tool_router: ToolRouter<Self>,
}

#[rmcp::tool_router]
impl McpServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[rmcp::tool(description = "Read SuperDuper DSP plugin status: loaded effect path, poisoned flag, current gain.")]
    async fn get_status(&self) -> Result<rmcp::Json<StatusOutput>, McpError> {
        let s = shared()?;
        Ok(rmcp::Json(StatusOutput {
            instance_id: s.instance_id.to_string(),
            loaded: s.slot.is_loaded(),
            poisoned: s.slot.is_poisoned(),
            effect_dylib_path: s.effect_dylib_path.display().to_string(),
            gain_db: s.gain_db.load(Ordering::Relaxed),
            bypassed: s.bypass.load(Ordering::Relaxed),
        }))
    }

    #[rmcp::tool(description = "List all plugin parameters (host Gain + effect-defined params) with current values.")]
    async fn get_params(&self) -> Result<rmcp::Json<ParamsOutput>, McpError> {
        let s = shared()?;
        let mut params = Vec::new();
        params.push(ParamView {
            name: "gain".into(),
            value: s.gain_db.load(Ordering::Relaxed),
            min: crate::PARAM_GAIN_MIN_DB as f32,
            max: crate::PARAM_GAIN_MAX_DB as f32,
            unit: "dB".into(),
        });
        let meta = s.slot.meta();
        for (i, p) in meta.params.iter().enumerate() {
            let value = s
                .effect_params
                .get(i)
                .map(|a| a.load(Ordering::Relaxed))
                .unwrap_or(0.0);
            params.push(ParamView {
                name: p.name.clone(),
                value,
                min: p.min,
                max: p.max,
                unit: p.unit.clone(),
            });
        }
        Ok(rmcp::Json(ParamsOutput { params }))
    }

    #[rmcp::tool(description = "Set a parameter value by name. Use 'gain' for host Gain or any effect-defined name (e.g. 'GAIN', 'DRIVE'). Returns the clamped value actually applied.")]
    async fn set_param(
        &self,
        Parameters(SetParamInput { name, value }): Parameters<SetParamInput>,
    ) -> Result<rmcp::Json<SetParamOutput>, McpError> {
        let s = shared()?;
        if name.eq_ignore_ascii_case("gain") {
            let clamped = value.clamp(
                crate::PARAM_GAIN_MIN_DB as f32,
                crate::PARAM_GAIN_MAX_DB as f32,
            );
            s.gain_db.store(clamped, Ordering::Relaxed);
            dlog!("MCP set_param gain → {:+.2} dB", clamped);
            return Ok(rmcp::Json(SetParamOutput {
                ok: true,
                applied_value: clamped,
                note: "host gain".into(),
            }));
        }
        let meta = s.slot.meta();
        let Some((idx, p)) = meta.params.iter().enumerate().find(|(_, p)| p.name == name) else {
            return Ok(rmcp::Json(SetParamOutput {
                ok: false,
                applied_value: 0.0,
                note: format!("unknown parameter '{}'", name),
            }));
        };
        let clamped = value.clamp(p.min, p.max);
        if let Some(atom) = s.effect_params.get(idx) {
            atom.store(clamped, Ordering::Relaxed);
        }
        dlog!("MCP set_param effect[{}] {} → {:.4}", idx, name, clamped);
        Ok(rmcp::Json(SetParamOutput {
            ok: true,
            applied_value: clamped,
            note: format!("effect param idx {}", idx),
        }))
    }

    #[rmcp::tool(description = "Bypass or un-bypass the plugin. Audio passes through unchanged when bypassed.")]
    async fn bypass(
        &self,
        Parameters(BypassInput { enabled }): Parameters<BypassInput>,
    ) -> Result<rmcp::Json<BypassOutput>, McpError> {
        let s = shared()?;
        s.bypass.store(enabled, Ordering::Relaxed);
        dlog!("MCP bypass → {}", enabled);
        Ok(rmcp::Json(BypassOutput { bypassed: enabled }))
    }

    #[rmcp::tool(description = "List every effect that has been compiled in this plugin's local build cache (\
~/.superduper-dsp/effect-builds/). Each entry has its name, a short preview of its source, and whether \
the compiled dylib exists on disk. Use this to remember what effects you've already authored, what their \
contracts looked like, and which ones are ready to hot-load.")]
    async fn list_effects(&self) -> Result<rmcp::Json<ListEffectsOutput>, McpError> {
        let s = shared()?;
        let builds_root = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
            .join(".superduper-dsp")
            .join("effect-builds");

        let mut effects = Vec::new();
        if builds_root.is_dir() {
            let mut entries: Vec<_> = std::fs::read_dir(&builds_root)
                .map(|it| it.flatten().collect())
                .unwrap_or_default();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let src = path.join("src").join("lib.rs");
                let source = std::fs::read_to_string(&src).unwrap_or_default();
                let params = parse_params_from_source(&source);
                // Check both host arches for the built dylib.
                let dylib_name = format!("libeffect_{}.dylib", sanitize_name(&name));
                let mut dylib_path = None;
                for triple in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
                    let candidate = path
                        .join("target")
                        .join(triple)
                        .join("release")
                        .join(&dylib_name);
                    if candidate.exists() {
                        dylib_path = Some(candidate.display().to_string());
                        break;
                    }
                }
                effects.push(EffectListing {
                    name,
                    built: dylib_path.is_some(),
                    source,
                    params,
                    dylib_path,
                });
            }
        }

        Ok(rmcp::Json(ListEffectsOutput {
            current_effect_dylib: s.effect_dylib_path.display().to_string(),
            effects,
        }))
    }

    #[rmcp::tool(description = "Compile a Rust DSP effect from source and hot-load it into the plugin. \
The crate is generated with the superduper-dsp-sdk dependency, `cargo build --release` is run for the \
host architecture, and the resulting .dylib is copied into the live instance directory where the file \
watcher swaps it onto the audio thread. Returns the compile log (always — useful for fix-it iterations) \
and the install path on success.")]
    async fn load_effect(
        &self,
        Parameters(LoadEffectInput { name, code }): Parameters<LoadEffectInput>,
    ) -> Result<rmcp::Json<LoadEffectOutput>, McpError> {
        let s = shared()?;
        let dest = s.effect_dylib_path.clone();
        dlog!(
            "MCP load_effect: name={:?}, code len={}, dest={:?}",
            name,
            code.len(),
            dest
        );

        // Run the synchronous cargo build off the tokio worker thread so the
        // MCP server can still service other requests if needed. spawn_blocking
        // gives us a dedicated OS thread.
        let name_clone = name.clone();
        let result = tokio::task::spawn_blocking(move || {
            build_pipeline::build(&name_clone, &code)
        })
        .await
        .map_err(|e| McpError::internal_error(format!("build task join failed: {}", e), None))?;

        if !result.success {
            return Ok(rmcp::Json(LoadEffectOutput {
                success: false,
                log: result.log,
                installed_at: None,
            }));
        }
        let built = match result.dylib {
            Some(p) => p,
            None => {
                return Ok(rmcp::Json(LoadEffectOutput {
                    success: false,
                    log: format!("{}\nbuild ok but no dylib path reported", result.log),
                    installed_at: None,
                }));
            }
        };

        // Make sure the target dir exists; the plugin creates it in
        // PluginShared::new but a manual `rm -rf` between sessions can wipe it.
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if let Err(e) = build_pipeline::install_dylib(&built, &dest) {
            return Ok(rmcp::Json(LoadEffectOutput {
                success: false,
                log: format!("{}\ninstall failed: {}", result.log, e),
                installed_at: None,
            }));
        }

        // Watcher will pick this up within ~250ms and swap automatically.
        // We could also force-trigger via `s.slot.swap(&dest)` here to be
        // instant, but doing it from a tokio worker means we hit the same
        // codepath external file changes use.
        dlog!("MCP load_effect: copied {:?} -> {:?}", built, dest);
        Ok(rmcp::Json(LoadEffectOutput {
            success: true,
            log: result.log,
            installed_at: Some(dest.display().to_string()),
        }))
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[rmcp::tool_handler]
impl ServerHandler for McpServer {
    fn get_info(&self) -> rmcp::model::ServerInfo {
        let server_info = rmcp::model::Implementation::new("superduper-dsp", env!("CARGO_PKG_VERSION"))
            .with_title("SuperDuper DSP")
            .with_website_url("https://github.com/fortunto2/superduper-dsp");
        rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_protocol_version(rmcp::model::ProtocolVersion::V_2025_03_26)
        .with_server_info(server_info)
        .with_instructions(
            "AI-authored DSP for REAPER via CLAP. Use get_status/get_params to introspect, \
             set_param to drive parameters live, bypass to A/B the effect.",
        )
    }
}

// ===========================================================================
// Server lifecycle (spawn / shutdown)
// ===========================================================================

/// RAII guard for the MCP server. Dropping it triggers graceful shutdown and
/// joins the runtime thread.
pub struct McpHandle {
    cancel: CancellationToken,
    thread: Option<std::thread::JoinHandle<()>>,
    pub url: String,
}

impl McpHandle {
    /// Build a no-op handle. Used when server start failed but we still want
    /// to consume the `OnceLock` slot so we don't retry forever.
    pub fn stub() -> Self {
        Self {
            cancel: CancellationToken::new(),
            thread: None,
            url: "(failed to start)".into(),
        }
    }
}

impl Drop for McpHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Path where we write `mcp-url.txt` so external tooling (Claude Code) can
/// discover the random port we bound on.
fn mcp_url_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".superduper-dsp")
        .join("mcp-url.txt")
}

/// Start the MCP server on a dedicated OS thread. Blocks the calling thread
/// briefly while it binds the TCP listener so we can return the URL.
pub fn start() -> std::io::Result<McpHandle> {
    let cancel = CancellationToken::new();
    let (tx, rx) = std::sync::mpsc::channel::<std::io::Result<String>>();

    let cancel_for_thread = cancel.clone();
    let thread = std::thread::Builder::new()
        .name("sdsp-mcp".into())
        .spawn(move || run_server(cancel_for_thread, tx))?;

    // Wait for bind result.
    let url = rx
        .recv()
        .map_err(|e| std::io::Error::other(format!("mcp init channel closed: {}", e)))??;

    // Persist URL for external discovery. Best-effort.
    let _ = std::fs::write(mcp_url_path(), &url);
    dlog!("MCP server listening on {}", url);

    Ok(McpHandle {
        cancel,
        thread: Some(thread),
        url,
    })
}

fn run_server(
    cancel: CancellationToken,
    tx: std::sync::mpsc::Sender<std::io::Result<String>>,
) {
    // Single-thread tokio runtime — small footprint, no work stealing.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = tx.send(Err(e));
            return;
        }
    };

    rt.block_on(async move {
        // Prefer fixed port 7891 (matches SPEC.md + .mcp.json) so the URL is
        // stable across plugin restarts. Fall back to a kernel-assigned port
        // if 7891 is in use (e.g. another DAW already hosting the plugin).
        let bind = match tokio::net::TcpListener::bind("127.0.0.1:7891").await {
            Ok(l) => l,
            Err(_) => match tokio::net::TcpListener::bind("127.0.0.1:0").await {
                Ok(l) => l,
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            },
        };
        let addr = match bind.local_addr() {
            Ok(a) => a,
            Err(e) => {
                let _ = tx.send(Err(e));
                return;
            }
        };
        let url = format!("http://{}/mcp", addr);
        let _ = tx.send(Ok(url));

        // Stateful streamable HTTP (the rmcp default). Stateless mode
        // (json_response: true + stateful_mode: false) caused Empty replies
        // on tool/call in our env — fall back to a session-per-init flow.
        // Claude Code reconnects cleanly on the next session start.
        let service = StreamableHttpService::new(
            || Ok(McpServer::new()),
            std::sync::Arc::new(LocalSessionManager::default()),
            StreamableHttpServerConfig::default()
                .with_sse_keep_alive(None)
                .with_cancellation_token(cancel.clone()),
        );
        let router = axum::Router::new().nest_service("/mcp", service);

        let _ = axum::serve(bind, router)
            .with_graceful_shutdown(async move { cancel.cancelled_owned().await })
            .await;
        dlog!("MCP server stopped");
    });
}
