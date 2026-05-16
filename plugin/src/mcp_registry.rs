//! Process-global registry that binds the in-plugin MCP server to one
//! "primary" `PluginShared` instance.
//!
//! Why a static registry? CLAP gives us `PluginShared` as a value type owned
//! by the host (lifetime `'a` tied to the plugin instance). We need *one*
//! long-lived MCP HTTP server for the whole REAPER process, and we need it
//! to talk to the audio plugin's state from a worker thread. Two options:
//!
//! 1. Wrap `PluginShared` in an `Arc` and hand the worker its own clone.
//!    Doesn't work cleanly with `clack-plugin`'s ownership model — the host
//!    decides where Shared lives.
//! 2. Stash a raw `*const PluginShared` for the first instance and have the
//!    MCP server deref through it. We pick this — see safety notes below.
//!
//! Safety
//! ------
//! - The pointer points to memory the host owns. The host may drop the
//!   instance at any time. We mitigate by:
//!   * The pointer is only set on the very first `PluginShared::new()` call.
//!     Subsequent instances (re-instantiations on the same track, or
//!     additional tracks) are *not* re-bound — they're invisible to MCP. This
//!     is the v0.1 single-instance mode declared in `SPEC.md`.
//!   * Any code that deref's the pointer does so synchronously from a tokio
//!     task on the MCP server thread. If the host has already dropped the
//!     primary instance, the pointer is stale. v0.2 will move to a proper
//!     `Arc<PluginShared>` shared-ownership model with a Drop-based
//!     unregister; for now we accept the limitation.
//! - All access goes through atomic operations (`AtomicF32`, `AtomicBool`,
//!   `ArcSwap<EffectMeta>`), so reads/writes are race-free even if the
//!   instance is in the middle of teardown.

use crate::{PluginShared, dbg_log, mcp_server};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, Ordering};

macro_rules! dlog { ($($arg:tt)*) => { dbg_log(format_args!($($arg)*)) } }

/// Raw pointer to the primary PluginShared. `null` until the first instance
/// initialises.
static PRIMARY: AtomicPtr<PluginShared> = AtomicPtr::new(std::ptr::null_mut());

/// MCP server handle for the whole process. Started lazily on first registration.
static MCP_HANDLE: OnceLock<mcp_server::McpHandle> = OnceLock::new();

/// Called from `new_shared` for every fresh PluginShared.
/// First call wins — its pointer becomes the MCP server's target.
pub fn register_first(s: &PluginShared) {
    let raw = s as *const PluginShared as *mut PluginShared;
    if PRIMARY
        .compare_exchange(
            std::ptr::null_mut(),
            raw,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        dlog!("mcp_registry: bound primary instance {}", s.instance_id);
        // Boot the MCP server. `get_or_init` makes this idempotent across
        // even pathological double-init races.
        let _ = MCP_HANDLE.get_or_init(|| match mcp_server::start() {
            Ok(h) => {
                dlog!("mcp_registry: MCP server URL → {}", h.url);
                h
            }
            Err(e) => {
                dlog!("mcp_registry: MCP server failed to start: {}", e);
                // Return a no-op handle so we don't try again every time.
                mcp_server::McpHandle::stub()
            }
        });
    }
}

/// Read the primary instance. Returns `None` until something registered.
///
/// # Safety
///
/// Caller must not retain the returned reference across an `await` point —
/// the primary instance can be dropped by the host between observation and
/// access. In practice tool handlers grab the pointer at the top of a `call`
/// and finish synchronously inside it.
pub fn primary() -> Option<&'static PluginShared> {
    let raw = PRIMARY.load(Ordering::Acquire);
    if raw.is_null() {
        None
    } else {
        // SAFETY: pointer is valid as long as the host holds the primary
        // instance. We trade safety for v0.2 ergonomics — see module docs.
        Some(unsafe { &*raw })
    }
}
