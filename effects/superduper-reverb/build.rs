// All build-time metadata is provided by the workspace's shared sdk-build crate.
// Add new env vars there once; every plugin gets them.
fn main() {
    superduper_dsp_sdk_build::emit_build_meta();
}
