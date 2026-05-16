//! Hot-reload slot for the user effect dylib.
//!
//! # Threading invariants
//!
//! - **Audio thread** only ever calls [`HotReloadSlot::call`]. That call:
//!   - Reads `current_fn` with `Acquire` (one atomic load).
//!   - Reads `poisoned` with `Relaxed` (one atomic load).
//!   - Invokes the user FFI inside `std::panic::catch_unwind`.
//!   - Never allocates, never locks, never blocks.
//!   - On panic, flips `poisoned` and falls through to passthrough.
//!
//! - **Worker threads** (file watcher, MCP load_effect, integration tests) call
//!   [`HotReloadSlot::swap`]:
//!   - `dlopen` the new dylib via `libloading`.
//!   - Optionally verify `sdsp_protocol_version` ABI handshake.
//!   - `dlsym` the `process` symbol.
//!   - Atomically swap the function pointer (`Release`).
//!   - Push the old `Library` into a grace-period queue.
//!   - Clear `poisoned`.
//!
//! - A separate periodic GC drops grace-period-expired libraries. Without this,
//!   memory leaks one `Library` handle per swap.
//!
//! # Why delayed drop?
//!
//! The audio thread may load `current_fn` just before a `swap`, then call into
//! the (still-pointing-to-the-old-dylib) function. If we `drop` the old
//! `Library` immediately, `dlclose` runs the dylib's code out from under us —
//! segfault. The grace period (200ms ≈ 3-4 typical audio buffer durations at
//! 48kHz/512) ensures the audio thread has long since returned before the
//! library is unmapped.

use arc_swap::ArcSwap;
use libloading::{Library, Symbol};
use parking_lot::Mutex;
use std::ffi::CStr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::time::{Duration, Instant};

/// ABI-stable param descriptor, mirrors `superduper_dsp_sdk::ParamMeta`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ParamMetaAbi {
    pub name: *const u8,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: *const u8,
}

/// One parameter exposed by the loaded effect, in Rust-owned form (so it
/// survives dylib swaps).
#[derive(Debug, Clone)]
pub struct EffectParam {
    pub name: String,
    pub unit: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
}

/// Full metadata snapshot for the currently loaded effect. Replaced atomically
/// via `ArcSwap<EffectMeta>` on every `swap()`.
#[derive(Debug, Clone, Default)]
pub struct EffectMeta {
    pub params: Vec<EffectParam>,
}

/// ABI signature exported by every effect dylib.
pub type ProcessFn = unsafe extern "C" fn(
    input: *const f32,
    output: *mut f32,
    channel_count: u32,
    frame_count: u32,
    params: *const f32,
);

/// ABI version the plugin requires. Effects expose this via
/// `sdsp_protocol_version() -> u32`. Bump when the contract changes.
pub const SDSP_PROTOCOL_VERSION: u32 = 1;

/// How long an old `Library` is kept alive after being swapped out.
///
/// 500ms covers the worst-case buffer durations we'll realistically see:
/// - 48kHz / 64 frames → 1.3ms/cb → 380+ callbacks
/// - 44.1kHz / 1024 → 23ms/cb → 21 callbacks
/// - 96kHz / 2048 → 21ms/cb → 23 callbacks
/// Plenty of margin even on slow buffers. Memory cost: a few KB per held lib.
const GRACE_PERIOD: Duration = Duration::from_millis(500);

#[derive(thiserror::Error, Debug)]
pub enum SwapError {
    #[error("failed to load dylib: {0}")]
    Load(#[from] libloading::Error),
    #[error("dylib reported protocol version {found}, plugin requires {required}")]
    ProtocolMismatch { found: u32, required: u32 },
    #[error("dylib is missing required export: {0}")]
    MissingSymbol(&'static str),
}

pub struct HotReloadSlot {
    current_fn: AtomicPtr<()>,
    poisoned: AtomicBool,
    libs: Mutex<Vec<(Instant, Library)>>,
    /// Latest known effect metadata. Read by main thread (CLAP params extension),
    /// written by worker threads on `swap()`. `ArcSwap` is lock-free on read.
    meta: ArcSwap<EffectMeta>,
    /// Flips to true after each successful `swap()`. Main thread observes it,
    /// triggers `host.params.rescan(ALL)` on the next callback, clears the flag.
    pub rescan_needed: AtomicBool,
}

impl HotReloadSlot {
    pub fn new() -> Self {
        Self {
            current_fn: AtomicPtr::new(std::ptr::null_mut()),
            poisoned: AtomicBool::new(false),
            libs: Mutex::new(Vec::new()),
            meta: ArcSwap::from_pointee(EffectMeta::default()),
            rescan_needed: AtomicBool::new(false),
        }
    }

    /// Lock-free read of the current effect metadata snapshot.
    pub fn meta(&self) -> Arc<EffectMeta> {
        self.meta.load_full()
    }

    /// Load a fresh dylib, verify the ABI handshake, and atomically replace the
    /// current `process` pointer.
    ///
    /// Called from a non-audio thread. Holds a mutex while pushing into the
    /// grace queue — audio thread never touches this mutex, so no priority
    /// inversion.
    pub fn swap(&self, dylib_path: &Path) -> Result<(), SwapError> {
        // SAFETY: libloading::Library::new is safe to call as long as the dylib's
        // initializers don't violate memory safety. We trust effects built from
        // our own workspace; rogue dylibs are the user's responsibility.
        let lib = unsafe { Library::new(dylib_path) }?;

        // Handshake — check protocol version.
        let version: u32 = unsafe {
            let sym: Symbol<unsafe extern "C" fn() -> u32> = lib
                .get(b"sdsp_protocol_version\0")
                .map_err(|_| SwapError::MissingSymbol("sdsp_protocol_version"))?;
            sym()
        };
        if version != SDSP_PROTOCOL_VERSION {
            return Err(SwapError::ProtocolMismatch {
                found: version,
                required: SDSP_PROTOCOL_VERSION,
            });
        }

        // dlsym the process function and erase its lifetime — the Library is
        // kept alive in `self.libs` for as long as the pointer can be in use.
        let raw_fn: *mut () = unsafe {
            let sym: Symbol<ProcessFn> = lib
                .get(b"process\0")
                .map_err(|_| SwapError::MissingSymbol("process"))?;
            *sym as *mut ()
        };

        // Read parameter metadata. `sdsp_param_count` / `sdsp_param_meta` are
        // exported by the SDK's `setup!()` macro. We copy strings out via
        // `CStr` so the resulting Vec survives dylib drop.
        let meta = unsafe { read_effect_meta(&lib) };

        // Atomic publish to audio thread.
        let _old_raw = self.current_fn.swap(raw_fn, Ordering::Release);
        self.meta.store(Arc::new(meta));
        self.poisoned.store(false, Ordering::Release);
        self.rescan_needed.store(true, Ordering::Release);

        // Keep the dylib alive past the grace period. GC sweeper drops it later.
        let mut guard = self.libs.lock();
        guard.push((Instant::now(), lib));
        Ok(())
    }

    /// Drop libraries whose grace period has expired.
    ///
    /// Always retains the most recent entry — that's the one `current_fn` still
    /// points into. Safe to call from any non-audio thread; cheap when the
    /// queue is short.
    pub fn gc(&self) {
        let mut guard = self.libs.lock();
        let len = guard.len();
        if len <= 1 {
            return;
        }
        let now = Instant::now();
        let last_idx = len - 1;
        let mut idx = 0;
        guard.retain(|(t, _)| {
            let keep = idx == last_idx || now.duration_since(*t) < GRACE_PERIOD;
            idx += 1;
            keep
        });
    }

    /// Audio-thread entry point.
    ///
    /// Returns `Err(())` when the slot is empty or poisoned — caller should
    /// fall back to passthrough. On panic from the loaded effect, the slot is
    /// poisoned and `Err(())` is returned (current and subsequent calls).
    ///
    /// # Safety
    ///
    /// `input`/`output` must be valid for `channel_count * frame_count` f32
    /// reads/writes. `params` must be valid for the effect's declared param
    /// count (0 in M1).
    #[inline]
    pub unsafe fn call(
        &self,
        input: *const f32,
        output: *mut f32,
        channel_count: u32,
        frame_count: u32,
        params: *const f32,
    ) -> Result<(), ()> {
        if self.poisoned.load(Ordering::Relaxed) {
            return Err(());
        }
        let raw = self.current_fn.load(Ordering::Acquire);
        if raw.is_null() {
            return Err(());
        }
        // SAFETY: pointer was produced by dlsym of an `extern "C" fn(...)`
        // with the same signature as ProcessFn, and the corresponding Library
        // is still alive (grace-period invariant).
        let process_fn: ProcessFn = core::mem::transmute(raw);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            process_fn(input, output, channel_count, frame_count, params);
        }));
        if result.is_err() {
            self.poisoned.store(true, Ordering::Release);
            return Err(());
        }
        Ok(())
    }

    /// True when the loaded effect has panicked at least once since the last swap.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned.load(Ordering::Acquire)
    }

    /// True when at least one effect has been successfully swapped in.
    pub fn is_loaded(&self) -> bool {
        !self.current_fn.load(Ordering::Acquire).is_null()
    }
}

impl Default for HotReloadSlot {
    fn default() -> Self {
        Self::new()
    }
}

/// Read `sdsp_param_count` + `sdsp_param_meta(i)` from the dylib, copy out
/// names/units into owned `String`s, return an `EffectMeta`.
///
/// # Safety
///
/// Caller holds `lib` alive during the call. The dylib's ABI exports must
/// match the contract described in `superduper-dsp-sdk`. If the symbols are
/// missing we silently return an empty `EffectMeta` — that's the M1 case
/// (effect built without `setup!()`).
unsafe fn read_effect_meta(lib: &Library) -> EffectMeta {
    let count: Symbol<unsafe extern "C" fn() -> u32> = match lib.get(b"sdsp_param_count\0") {
        Ok(s) => s,
        Err(_) => return EffectMeta::default(),
    };
    let meta_fn: Symbol<unsafe extern "C" fn(u32) -> ParamMetaAbi> =
        match lib.get(b"sdsp_param_meta\0") {
            Ok(s) => s,
            Err(_) => return EffectMeta::default(),
        };

    let n = count();
    let mut params = Vec::with_capacity(n as usize);
    for i in 0..n {
        let m = meta_fn(i);
        let name = c_str_to_string(m.name);
        let unit = c_str_to_string(m.unit);
        params.push(EffectParam {
            name,
            unit,
            min: m.min,
            max: m.max,
            default: m.default,
        });
    }
    EffectMeta { params }
}

unsafe fn c_str_to_string(p: *const u8) -> String {
    if p.is_null() {
        return String::new();
    }
    CStr::from_ptr(p as *const i8)
        .to_string_lossy()
        .into_owned()
}

// SAFETY: All mutation happens via atomics or a Mutex. `Library` is Send+Sync
// per its docs. No `&self` accessor leaks a reference into the `Library` —
// dlsym'd pointers are `*mut ()` with manual lifetime management.
unsafe impl Send for HotReloadSlot {}
unsafe impl Sync for HotReloadSlot {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_slot_call_returns_err() {
        let slot = HotReloadSlot::new();
        let input = [0.5_f32; 4];
        let mut output = [0.0_f32; 4];
        let result = unsafe {
            slot.call(
                input.as_ptr(),
                output.as_mut_ptr(),
                1,
                4,
                std::ptr::null(),
            )
        };
        assert!(result.is_err());
        assert!(!slot.is_loaded());
        assert!(!slot.is_poisoned());
    }

    #[test]
    fn poisoned_slot_call_returns_err() {
        let slot = HotReloadSlot::new();
        slot.poisoned.store(true, Ordering::Release);
        let input = [0.5_f32; 4];
        let mut output = [0.0_f32; 4];
        let result = unsafe {
            slot.call(
                input.as_ptr(),
                output.as_mut_ptr(),
                1,
                4,
                std::ptr::null(),
            )
        };
        assert!(result.is_err());
        assert!(slot.is_poisoned());
    }
}
