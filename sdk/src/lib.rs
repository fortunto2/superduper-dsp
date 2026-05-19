//! SuperDuper DSP SDK — utilities for writing DSP effects.
//!
//! Every user effect crate depends on this and exposes a `process` C-ABI symbol.
//! Parameters declared via `params!` macro are read by the daemon through stable
//! ABI export functions injected by `setup!()`.
//!
//! # Minimal example
//!
//! ```ignore
//! use superduper_dsp_sdk::*;
//!
//! setup!();
//!
//! params! {
//!     DRIVE = param(0.0, 1.0).default(0.5).unit("x"),
//!     TONE  = param(-1.0, 1.0).default(0.0),
//! }
//!
//! #[no_mangle]
//! pub extern "C" fn process(
//!     input: *const f32,
//!     output: *mut f32,
//!     channel_count: u32,
//!     frame_count: u32,
//!     params: *const f32,
//! ) {
//!     let drive = unsafe { *params.add(DRIVE) };
//!     let tone = unsafe { *params.add(TONE) };
//!
//!     for ch in 0..channel_count as usize {
//!         for i in 0..frame_count as usize {
//!             let idx = ch * frame_count as usize + i;
//!             let x = unsafe { *input.add(idx) };
//!             unsafe { *output.add(idx) = (x * (1.0 + drive * 4.0)).tanh() };
//!         }
//!     }
//! }
//! ```

// NOTE: SDK is `std` for now — `f32::exp/tanh/powf` come from `std`. If we ever
// need true `no_std` for embedded targets, pull in `libm` and swap method calls
// for `libm::expf`/`libm::tanhf`. For now, every effect is a normal cdylib that
// already links std, so #![no_std] would buy us nothing.

pub mod dsp;
pub mod build_meta;
pub mod clap_helpers;
pub mod log;

/// Stamp out the standard `PluginStateImpl` block for a plugin whose
/// state is exactly "all CLAP params + the bypass flag" — i.e. it
/// doesn't carry any custom non-param data (no harmonic curves, no
/// drawn waveforms, no MIDI-learn map).
///
/// Before this macro every such plugin (~13 of them) copy-pasted the
/// same 15-line `impl PluginStateImpl for PluginMainThread<'_> { save,
/// load }` block. Call sites now collapse to:
///
/// ```ignore
/// superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);
/// ```
///
/// Plugins with custom state (Kubyz, Wave) keep their hand-rolled
/// JSON impl and just don't invoke the macro.
#[macro_export]
macro_rules! simple_state_impl {
    ($ty:ty) => {
        impl ::clack_extensions::state::PluginStateImpl for $ty {
            fn save(
                &mut self,
                output: &mut ::clack_common::stream::OutputStream,
            ) -> Result<(), ::clack_plugin::prelude::PluginError> {
                $crate::clap_helpers::save_simple_state(
                    &self.shared.params,
                    self.shared.bypass.load(::std::sync::atomic::Ordering::Relaxed),
                    output,
                )
            }
            fn load(
                &mut self,
                input: &mut ::clack_common::stream::InputStream,
            ) -> Result<(), ::clack_plugin::prelude::PluginError> {
                let bypass =
                    $crate::clap_helpers::load_simple_state(&self.shared.params, input)?;
                self.shared
                    .bypass
                    .store(bypass, ::std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
        }
    };
}

// ============================================================================
// ABI types for daemon ↔ dylib metadata exchange
// ============================================================================

/// Parameter metadata as exposed to the daemon through stable C ABI.
/// All string pointers are null-terminated C strings with `'static` lifetime
/// (owned by the dylib's static data).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ParamMeta {
    pub name: *const u8,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    /// Null pointer if no unit.
    pub unit: *const u8,
}

// Safety: ParamMeta is only ever read, and the strings it points to are static.
unsafe impl Sync for ParamMeta {}

// ============================================================================
// Builder used inside params! macro
// ============================================================================

pub struct ParamBuilder {
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub unit: Option<&'static str>,
}

impl ParamBuilder {
    pub const fn default(mut self, v: f32) -> Self {
        self.default = v;
        self
    }

    pub const fn unit(mut self, u: &'static str) -> Self {
        self.unit = Some(u);
        self
    }
}

/// Start a parameter declaration. Used inside `params! { ... }`.
pub const fn param(min: f32, max: f32) -> ParamBuilder {
    ParamBuilder {
        min,
        max,
        default: 0.0,
        unit: None,
    }
}

// ============================================================================
// Macros
// ============================================================================

/// Inject the stable ABI export functions that the daemon reads.
/// Call once at the top of your effect crate's `process.rs`.
#[macro_export]
macro_rules! setup {
    () => {
        #[no_mangle]
        pub extern "C" fn sdsp_protocol_version() -> u32 {
            1
        }

        #[no_mangle]
        pub extern "C" fn sdsp_param_count() -> u32 {
            __PARAM_METAS.len() as u32
        }

        #[no_mangle]
        pub extern "C" fn sdsp_param_meta(idx: u32) -> $crate::ParamMeta {
            if (idx as usize) < __PARAM_METAS.len() {
                __PARAM_METAS[idx as usize]
            } else {
                $crate::ParamMeta {
                    name: core::ptr::null(),
                    min: 0.0,
                    max: 0.0,
                    default: 0.0,
                    unit: core::ptr::null(),
                }
            }
        }
    };
}

/// Declare effect parameters with metadata.
///
/// Proc-macro form (M2) — emits:
/// - `pub const <NAME>: usize = <index>` for each param
/// - `pub const __PARAM_COUNT: usize = N`
/// - `static __PARAM_NAME_i: &[u8]` (null-terminated)
/// - `static __PARAM_UNIT_i: &[u8]` (null-terminated, empty if no unit)
/// - `static __PARAM_METAS: [ParamMeta; N]` (consumed by `setup!()`)
///
/// Usage:
/// ```ignore
/// params! {
///     GAIN  = param(-24.0, 24.0).default(0.0).unit("dB"),
///     DRIVE = param(0.0, 1.0).default(0.5),
/// }
/// ```
pub use superduper_dsp_sdk_macros::params;
