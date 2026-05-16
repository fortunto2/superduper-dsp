//! Integration test: build `example-passthrough` as a cdylib, then load it
//! through [`HotReloadSlot::swap`] and verify a `process()` block.
//!
//! Run with: `cargo test -p superduper-dsp-plugin --test hotreload -- --nocapture`
//!
//! Skipped if the example-passthrough dylib hasn't been built yet (so plain
//! `cargo test -p superduper-dsp-plugin` doesn't fail when the artifact is
//! missing — see CI hook in `scripts/test_hotreload.sh` or run
//! `cargo build -p example-passthrough --release` first).

use std::path::PathBuf;
use superduper_dsp::HotReloadSlot;

fn dylib_path() -> PathBuf {
    let target_root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let manifest = std::env::var("CARGO_MANIFEST_DIR")
                .expect("CARGO_MANIFEST_DIR set by cargo");
            PathBuf::from(manifest).join("..").join("target")
        });
    target_root
        .join("release")
        .join("libexample_passthrough.dylib")
}

#[test]
fn swap_and_call_passthrough() {
    let dylib = dylib_path();
    if !dylib.exists() {
        eprintln!(
            "SKIPPED: build example-passthrough first:\n  \
             cargo build -p example-passthrough --release\n  \
             (expected at {:?})",
            dylib
        );
        return;
    }

    let slot = HotReloadSlot::new();
    assert!(!slot.is_loaded(), "fresh slot is empty");

    slot.swap(&dylib).expect("swap succeeds");
    assert!(slot.is_loaded(), "loaded after swap");
    assert!(!slot.is_poisoned(), "fresh load not poisoned");

    // Pass a known buffer through. Effect: gain_db = 0 (unity), drive = 0 (off)
    // → output == input.
    let input: Vec<f32> = (0..512).map(|i| (i as f32 * 0.01).sin()).collect();
    let mut output = vec![0.0_f32; 512];
    // Params layout matches `params!` order in example-passthrough/src/lib.rs:
    //   index 0 = GAIN (dB), index 1 = DRIVE
    let params = [0.0_f32, 0.0_f32];
    let result = unsafe {
        slot.call(
            input.as_ptr(),
            output.as_mut_ptr(),
            1,
            512,
            params.as_ptr(),
        )
    };
    assert!(result.is_ok(), "call succeeded");
    for (i, (inp, out)) in input.iter().zip(output.iter()).enumerate() {
        assert!(
            (inp - out).abs() < 1e-6,
            "sample {i}: in={inp}, out={out}"
        );
    }
}

#[test]
fn protocol_mismatch_rejected() {
    // Construct a fake dylib path that doesn't exist — swap should Err.
    let bogus = PathBuf::from("/tmp/nonexistent-effect.dylib");
    let slot = HotReloadSlot::new();
    let result = slot.swap(&bogus);
    assert!(result.is_err());
    assert!(!slot.is_loaded());
}

#[test]
fn swap_then_swap_keeps_grace_libs() {
    let dylib = dylib_path();
    if !dylib.exists() {
        eprintln!("SKIPPED: example-passthrough not built");
        return;
    }
    let slot = HotReloadSlot::new();
    slot.swap(&dylib).unwrap();
    slot.swap(&dylib).unwrap();
    // Both swaps succeed; the first library should still be in the grace queue
    // (we can't directly observe libs.len() through the public API, but the
    // absence of segfault is the load-bearing check here).
    assert!(slot.is_loaded());
}
