//! `clap.preset-load/2` — how a CLAP host recalls one of a plugin's own
//! presets: location kind PLUGIN, `load_key` = the preset's index or name.
//!
//! The Preset *parameter* is a label (a parameter that rewrote the other
//! parameters failed CLAP's parameter independence, clap-validator 0.4,
//! 2026-10-21). Recall has three doors, all explicit: this extension for
//! hosts (sdsp-chain renders `preset = "808 Sub"` through it), the `preset`
//! mailbox document for agents, the GUI's list for people. Each plugin
//! implements [`PluginPresetLoadImpl`] with its own `apply_preset` and
//! registers [`PluginPresetLoad`] next to its other extensions.

use clack_common::extensions::*;
use clap_sys::ext::preset_load::{clap_plugin_preset_load, CLAP_EXT_PRESET_LOAD, CLAP_EXT_PRESET_LOAD_COMPAT};
use std::ffi::{c_char, CStr};

/// `location_kind` for a preset that lives inside the plugin.
pub const LOCATION_PLUGIN: u32 = clap_sys::factory::preset_discovery::CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN;

#[derive(Copy, Clone)]
pub struct PluginPresetLoad(pub RawExtension<PluginExtensionSide, clap_plugin_preset_load>);

// SAFETY: the raw struct is `clap_plugin_preset_load` for both identifiers.
unsafe impl Extension for PluginPresetLoad {
    const IDENTIFIERS: &[&CStr] = &[CLAP_EXT_PRESET_LOAD, CLAP_EXT_PRESET_LOAD_COMPAT];
    type ExtensionSide = PluginExtensionSide;

    #[inline]
    unsafe fn from_raw(raw: RawExtension<Self::ExtensionSide>) -> Self {
        Self(unsafe { raw.cast() })
    }
}

/// Main thread. `load_key` is the preset's index or name; the host-side
/// `location` is `None` for a plugin-internal preset.
pub trait PluginPresetLoadImpl {
    fn from_location(&mut self, location_kind: u32, location: Option<&CStr>, load_key: Option<&CStr>) -> bool;
}

mod plugin {
    use super::*;
    use clack_plugin::extensions::prelude::*;

    // SAFETY: the vtable matches `clap_plugin_preset_load`.
    unsafe impl<P> ExtensionImplementation<P> for PluginPresetLoad
    where
        for<'a> P: Plugin<MainThread<'a>: PluginPresetLoadImpl>,
    {
        const IMPLEMENTATION: RawExtensionImplementation =
            RawExtensionImplementation::new(&clap_plugin_preset_load { from_location: Some(from_location::<P>) });
    }

    #[allow(clippy::missing_safety_doc)]
    unsafe extern "C" fn from_location<P>(
        plugin: *const clap_plugin,
        location_kind: u32,
        location: *const c_char,
        load_key: *const c_char,
    ) -> bool
    where
        for<'a> P: Plugin<MainThread<'a>: PluginPresetLoadImpl>,
    {
        PluginWrapper::<P>::handle(plugin, |plugin| {
            // SAFETY: the host passes valid C strings or null for the call's duration.
            let location = if location.is_null() { None } else { Some(unsafe { CStr::from_ptr(location) }) };
            let load_key = if load_key.is_null() { None } else { Some(unsafe { CStr::from_ptr(load_key) }) };
            Ok(plugin.main_thread().as_mut().from_location(location_kind, location, load_key))
        })
        .unwrap_or(false)
    }
}

/// The body every plugin's `from_location` shares: the key names one of
/// `names` (index or name), `apply` recalls it.
pub fn load_named(location_kind: u32, load_key: Option<&CStr>, names: &[&str], apply: impl FnOnce(usize)) -> bool {
    if location_kind != LOCATION_PLUGIN {
        return false;
    }
    let Some(key) = load_key.and_then(|k| k.to_str().ok()) else { return false };
    match crate::clap_helpers::preset_index(names, key) {
        Ok(i) => { apply(i); true }
        Err(_) => false,
    }
}
