//! clack-host wrapper — instantiates a CLAP plugin from a file path and
//! exposes a `LoadedPlugin` struct with the bits the rest of the runner
//! needs (the entry, factory descriptor metadata).
//!
//! The actual `PluginInstance` is created later in `audio.rs` once we know
//! the sample rate / block size cpal picked.

use anyhow::{Context, Result};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::prelude::*;
use std::ffi::CString;
use std::path::Path;

/// What we know about a successfully-loaded plugin before instantiation.
pub struct LoadedPlugin {
    pub entry: PluginEntry,
    pub descriptor_id: String,
    pub descriptor_name: String,
}

impl LoadedPlugin {
    /// CString form of the plugin id, ready to pass to `PluginInstance::new`.
    pub fn id_cstr(&self) -> CString {
        CString::new(self.descriptor_id.as_str()).unwrap()
    }
}

pub fn load_plugin(path: &Path) -> Result<LoadedPlugin> {
    // CLAP "bundles" on macOS are directories. clack-host expects either a
    // bundle directory or the inner dylib — `PluginEntry::load` handles both.
    // SAFETY: we trust the host who installed the plugin; this is the same
    // assumption REAPER / Bitwig make.
    let entry = unsafe { PluginEntry::load(path) }
        .context("PluginEntry::load failed (bad path or not a CLAP plugin?)")?;

    let factory = entry
        .get_factory::<clack_host::factory::plugin::PluginFactory>()
        .context("plugin exposes no PluginFactory")?;

    if factory.plugin_count() == 0 {
        anyhow::bail!("plugin factory is empty");
    }

    let desc = factory
        .plugin_descriptor(0)
        .context("could not read first plugin descriptor")?;
    let id = desc
        .id()
        .context("plugin has no id")?
        .to_str()
        .context("plugin id is not UTF-8")?
        .to_string();
    let name = desc
        .name()
        .context("plugin has no name")?
        .to_str()
        .context("plugin name is not UTF-8")?
        .to_string();

    Ok(LoadedPlugin {
        entry,
        descriptor_id: id,
        descriptor_name: name,
    })
}

// ---------------------------------------------------------------------------
// HostHandlers — minimal viable host. We forward plugin log messages to
// stderr and stub out everything else.
// ---------------------------------------------------------------------------

pub struct Host;
pub struct HostShared;

impl SharedHandler<'_> for HostShared {
    fn request_restart(&self) { eprintln!("[host] request_restart"); }
    fn request_process(&self) {}
    fn request_callback(&self) {}
}

impl HostLogImpl for HostShared {
    fn log(&self, severity: LogSeverity, message: &str) {
        eprintln!("[plugin {severity}] {message}");
    }
}

impl HostHandlers for Host {
    type Shared<'a> = HostShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder.register::<HostLog>();
    }
}

pub fn host_info() -> Result<HostInfo> {
    HostInfo::new(
        "sdsp-runner",
        "SuperDuperAI",
        "https://github.com/fortunto2/superduper-dsp",
        "0.1.0",
    )
    .context("HostInfo::new failed")
}
