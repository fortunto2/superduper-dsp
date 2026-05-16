//! Inspect a freshly built .dylib for parameter metadata via stable ABI.
//!
//! M4 scope.
//!
//! Calls `sdsp_protocol_version`, `sdsp_param_count`, `sdsp_param_meta(idx)`
//! exported by the user dylib (injected by the `setup!()` macro from the SDK).

use anyhow::Result;
use std::path::Path;
use superduper_dsp_protocol::ParamInfo;

#[allow(dead_code)]
pub fn inspect_params(dylib_path: &Path) -> Result<Vec<ParamInfo>> {
    let _ = dylib_path;
    // TODO M4:
    // unsafe {
    //   let lib = libloading::Library::new(dylib_path)?;
    //   let version_fn: Symbol<extern "C" fn() -> u32> = lib.get(b"sdsp_protocol_version\0")?;
    //   if version_fn() != 1 { return Err(anyhow!("incompatible SDK version")); }
    //
    //   let count_fn: Symbol<extern "C" fn() -> u32> = lib.get(b"sdsp_param_count\0")?;
    //   let meta_fn: Symbol<extern "C" fn(u32) -> ParamMeta> = lib.get(b"sdsp_param_meta\0")?;
    //
    //   for i in 0..count_fn() {
    //     let meta = meta_fn(i);
    //     let name = cstr_to_string(meta.name);
    //     // ...
    //   }
    // }
    Ok(Vec::new())
}
