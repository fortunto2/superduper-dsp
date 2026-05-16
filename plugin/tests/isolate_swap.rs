//! Isolation: just `slot.swap(dylib)` with no `slot.call()`. Helps narrow
//! down whether SIGSEGV is in `read_effect_meta` (called from swap) or in
//! the actual `process()` invocation.

use std::path::PathBuf;
use superduper_dsp::HotReloadSlot;

#[test]
fn swap_only_loads_metadata() {
    let target_root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
            PathBuf::from(manifest).join("..").join("target")
        });
    let dylib = target_root.join("release/libexample_passthrough.dylib");
    if !dylib.exists() {
        eprintln!("SKIP: build example-passthrough first ({:?})", dylib);
        return;
    }
    eprintln!("LOADING {:?}", dylib);
    let slot = HotReloadSlot::new();
    eprintln!("ABOUT TO SWAP");
    match slot.swap(&dylib) {
        Ok(()) => eprintln!("SWAP OK"),
        Err(e) => panic!("swap failed: {}", e),
    }
    let meta = slot.meta();
    eprintln!("META: {} params", meta.params.len());
    for (i, p) in meta.params.iter().enumerate() {
        eprintln!(
            "  [{}] name={:?} unit={:?} min={} max={} default={}",
            i, p.name, p.unit, p.min, p.max, p.default
        );
    }
}
