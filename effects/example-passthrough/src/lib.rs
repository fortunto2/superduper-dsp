//! Reference effect: pure pass-through.
//!
//! M1: no params (params! macro pending proc-macro rewrite in M2). The plugin
//! host loads this dylib at runtime via `libloading`, calls `process()` from
//! the audio thread, and atomically swaps to a new compiled version when the
//! source changes — no DAW restart needed.
//!
//! Real-time contract:
//! - No allocations
//! - No locks
//! - No syscalls / I/O / println
//! - No panics (or it gets caught by `catch_unwind` and the instance is poisoned)

#![allow(clippy::missing_safety_doc)]

/// The host calls this each audio block.
///
/// `input` and `output` are flat interleaved planar buffers of length
/// `channel_count * frame_count` f32 samples. `params` is unused in this M1
/// example (no parameters declared).
///
/// # Safety
///
/// Host guarantees pointer validity for the given lengths.
#[no_mangle]
pub unsafe extern "C" fn process(
    input: *const f32,
    output: *mut f32,
    channel_count: u32,
    frame_count: u32,
    _params: *const f32,
) {
    let total = (channel_count as usize) * (frame_count as usize);
    for i in 0..total {
        *output.add(i) = *input.add(i);
    }
}

/// Stable ABI version handshake. Plugin refuses to load dylibs with a different
/// protocol number — protects against silent ABI drift across SDK versions.
#[no_mangle]
pub extern "C" fn sdsp_protocol_version() -> u32 {
    1
}

/// Parameter descriptor JSON (null-terminated). Empty list in M1.
///
/// Returned as a single `*const u8` so we avoid struct-ABI risks across versions.
#[no_mangle]
pub extern "C" fn sdsp_param_descriptor_json() -> *const u8 {
    // Static null-terminated empty JSON array.
    static JSON: &[u8] = b"[]\0";
    JSON.as_ptr()
}
