//! Reference effect: gain + soft-clip drive.
//!
//! Demonstrates the M2 contract:
//!   - `use superduper_dsp_sdk::*;` imports `ParamMeta`, `params!`, dsp helpers.
//!   - `params! { ... }` emits the parameter constants and the static
//!     `__PARAM_METAS` array consumed by `setup!()` macro / ABI exports.
//!   - `setup!()` from the SDK injects `sdsp_protocol_version`,
//!     `sdsp_param_count`, `sdsp_param_meta` exports.
//!   - `process` is the hot path — no alloc, no panic, no syscalls.

#![allow(clippy::missing_safety_doc)]

use superduper_dsp_sdk::*;

params! {
    GAIN  = param(-24.0, 24.0).default(0.0).unit("dB"),
    DRIVE = param(0.0, 1.0).default(0.0),
}

setup!();

#[no_mangle]
pub unsafe extern "C" fn process(
    input: *const f32,
    output: *mut f32,
    channel_count: u32,
    frame_count: u32,
    params: *const f32,
) {
    let gain_db = *params.add(GAIN);
    let drive = *params.add(DRIVE);
    let gain_linear = 10f32.powf(gain_db / 20.0);

    let total = (channel_count as usize) * (frame_count as usize);
    for i in 0..total {
        let x = *input.add(i);
        let y = if drive > 0.001 {
            dsp::soft_clip(x * gain_linear, drive)
        } else {
            x * gain_linear
        };
        *output.add(i) = y;
    }
}
