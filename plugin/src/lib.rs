//! SuperDuper DSP — CLAP plugin.
//!
//! M0: Hello CLAP + one `Gain` parameter.
//! M1: Native dylib hot-reload via [`HotReloadSlot`] + file watcher.
//!     Signal flow: input → effect.process() (if loaded) → gain → output.
//!     Effect panics are caught, instance is poisoned, audio keeps flowing.

#![allow(clippy::missing_safety_doc)]

mod hotreload;
mod watcher;

pub use hotreload::{HotReloadSlot, ProcessFn, SDSP_PROTOCOL_VERSION, SwapError};

use atomic_float::AtomicF32;
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_plugin::events::event_types::ParamValueEvent;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use uuid::Uuid;

/// Debug log file (`~/.superduper-dsp/plugin.log`). Plain append-only, not
/// RT-safe — strictly for development. Stderr in a Dock-spawned plugin host
/// disappears, so unified `log stream` won't see our `tracing` output.
static LOG_FILE: std::sync::OnceLock<parking_lot::Mutex<Option<std::fs::File>>> =
    std::sync::OnceLock::new();

fn init_logging() {
    LOG_FILE.get_or_init(|| {
        let path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".superduper-dsp")
            .join("plugin.log");
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        parking_lot::Mutex::new(file)
    });
    dbg_log(format_args!("=== plugin loaded ==="));
}

pub fn dbg_log(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    if let Some(slot) = LOG_FILE.get() {
        if let Some(file) = slot.lock().as_mut() {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let _ = writeln!(file, "[{}] {}", now, args);
        }
    }
}

/// Convenience: same call shape as `dlog!`, but writes to file.
macro_rules! dlog {
    ($($arg:tt)*) => { $crate::dbg_log(format_args!($($arg)*)) };
}

pub const PLUGIN_ID: &str = "co.superduperai.dsp";
pub const PLUGIN_NAME: &str = "SuperDuper DSP";
pub const PLUGIN_VENDOR: &str = "SuperDuperAI";
pub const PLUGIN_VERSION: &str = "0.1.0";

pub const PARAM_GAIN_ID: u32 = 0;
pub const PARAM_GAIN_MIN_DB: f64 = -24.0;
pub const PARAM_GAIN_MAX_DB: f64 = 24.0;
pub const PARAM_GAIN_DEFAULT_DB: f64 = 0.0;

const fn gain_clap_id() -> ClapId {
    // 0 is a valid ClapId — the type only forbids `u32::MAX`.
    ClapId::new(PARAM_GAIN_ID)
}

// ============================================================================
// Plugin state — shared between threads (audio + main)
// ============================================================================

/// State shared between the audio thread and the main thread.
///
/// `AtomicF32` so the audio thread can read with `Relaxed` ordering while the
/// main thread updates it from CLAP `flush()` / `process()` events.
///
/// `slot` is the hot-reload slot for the user effect. Reads from audio thread,
/// writes from the file-watcher thread (worker).
pub struct PluginShared {
    pub gain_db: AtomicF32,
    pub bypass: AtomicBool,
    pub slot: Arc<HotReloadSlot>,
    pub instance_id: Uuid,
    pub effect_dylib_path: PathBuf,
    /// Debug counters: number of times the audio thread entered `process()`
    /// and number of ParamValueEvents we observed. Logged periodically so we
    /// can see in REAPER's stderr whether events ever arrive.
    pub process_calls: AtomicU64,
    pub events_seen: AtomicU64,
    /// File watcher RAII guard, populated lazily by `ensure_watcher()` on the
    /// first `activate()` call. Dropped when PluginShared is dropped → watcher
    /// thread exits.
    _watcher: parking_lot::Mutex<Option<watcher::WatcherHandle>>,
}

impl PluginShared {
    pub fn new() -> Self {
        let instance_id = Uuid::new_v4();
        let slot = Arc::new(HotReloadSlot::new());

        // Per-instance directory: ~/.superduper-dsp/instances/<uuid>/effect.dylib
        let instance_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".superduper-dsp")
            .join("instances")
            .join(instance_id.to_string());

        // Best-effort: create the directory. If it fails we still start; watcher
        // just won't see anything until it exists.
        let _ = std::fs::create_dir_all(&instance_dir);

        let effect_dylib_path = instance_dir.join("effect.dylib");

        // NOTE: watcher init is deferred to `ensure_watcher()` called from
        // `activate()` on the audio thread setup. `notify::recommended_watcher`
        // can block briefly while initialising FSEvents on macOS, and during
        // CLAP plugin scan that latency made REAPER's main thread unresponsive
        // to subsequent API calls. By deferring, scan stays cheap.
        Self {
            gain_db: AtomicF32::new(PARAM_GAIN_DEFAULT_DB as f32),
            bypass: AtomicBool::new(false),
            slot,
            instance_id,
            effect_dylib_path,
            process_calls: AtomicU64::new(0),
            events_seen: AtomicU64::new(0),
            _watcher: parking_lot::Mutex::new(None),
        }
    }

    /// Lazy-init watcher on first audio activation. Idempotent.
    pub fn ensure_watcher(&self) {
        let mut guard = self._watcher.lock();
        if guard.is_some() {
            return;
        }

        // If a dylib already exists at startup (e.g. a prior session left one
        // behind), load it before the watcher starts.
        if self.effect_dylib_path.exists() {
            if let Err(e) = self.slot.swap(&self.effect_dylib_path) {
                tracing::warn!(
                    "initial swap of {:?} failed: {}",
                    self.effect_dylib_path,
                    e
                );
            }
        }

        match watcher::start(self.slot.clone(), self.effect_dylib_path.clone()) {
            Ok(handle) => *guard = Some(handle),
            Err(e) => tracing::warn!("file watcher did not start ({}); hot-reload disabled", e),
        }
    }

    #[inline]
    pub fn gain_linear(&self) -> f32 {
        let db = self.gain_db.load(Ordering::Relaxed);
        10f32.powf(db / 20.0)
    }
}

impl Default for PluginShared {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

// ============================================================================
// Main thread state
// ============================================================================

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

// ============================================================================
// Audio processor state
// ============================================================================

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
}

/// Apply param_value events from a CLAP event buffer to `shared`.
///
/// Used by both the audio processor (process + flush) and the main thread (flush).
fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    let target = gain_clap_id();
    for event in events {
        let Some(pv) = event.as_event::<ParamValueEvent>() else {
            continue;
        };
        let Some(id) = pv.param_id() else { continue };
        if id == target {
            let db = pv.value() as f32;
            shared.gain_db.store(db, Ordering::Relaxed);
            let count = shared.events_seen.fetch_add(1, Ordering::Relaxed) + 1;
            // Log every event (audio thread, but rare enough to be OK for debug).
            dlog!("ParamValueEvent #{}: gain → {:+.2} dB", count, db);
        }
    }
}

impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, PluginShared, PluginMainThread<'a>>
    for PluginAudioProcessor<'a>
{
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut PluginMainThread<'a>,
        shared: &'a PluginShared,
        _audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        // Lazy-start the file watcher now that the plugin is actually being
        // used (not just scanned). Idempotent.
        shared.ensure_watcher();
        Ok(Self { shared })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let n = self.shared.process_calls.fetch_add(1, Ordering::Relaxed) + 1;
        // Log every 1024 calls (≈22s at 48kHz/512). Cheap atomic increment.
        if n.is_multiple_of(1024) {
            let events_len = events.input.len();
            dlog!(
                "process #{}: events.len={}, events_seen={}, gain_db={:+.2}",
                n,
                events_len,
                self.shared.events_seen.load(Ordering::Relaxed),
                self.shared.gain_db.load(Ordering::Relaxed),
            );
        }
        apply_param_events(self.shared, events.input);

        if self.shared.bypass.load(Ordering::Relaxed) {
            for mut port_pair in &mut audio {
                if let Some(channel_pairs) = port_pair.channels()?.into_f32() {
                    for channel_pair in channel_pairs {
                        if let ChannelPair::InputOutput(input, output) = channel_pair {
                            for (i, o) in input.iter().zip(output) {
                                *o = *i;
                            }
                        }
                    }
                }
            }
            return Ok(ProcessStatus::Continue);
        }

        let gain = self.shared.gain_linear();
        let slot = &*self.shared.slot;

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else {
                continue;
            };
            for channel_pair in channel_pairs {
                match channel_pair {
                    ChannelPair::InputOnly(_) => {}
                    ChannelPair::OutputOnly(buf) => buf.fill(0.0),
                    ChannelPair::InputOutput(input, output) => {
                        // 1. Effect: input → output. If no effect / poisoned,
                        //    fall back to passthrough copy.
                        let effect_ran = unsafe {
                            slot.call(
                                input.as_ptr(),
                                output.as_mut_ptr(),
                                1,
                                input.len() as u32,
                                std::ptr::null(),
                            )
                        }
                        .is_ok();
                        if !effect_ran {
                            for (i, o) in input.iter().zip(output.iter_mut()) {
                                *o = *i;
                            }
                        }
                        // 2. Apply gain in-place on output.
                        for s in output.iter_mut() {
                            *s *= gain;
                        }
                    }
                    ChannelPair::InPlace(buf) => {
                        // M1: skip effect for InPlace (no tmp buffer without
                        // alloc). M2 will fix this via a stack-allocated tmp.
                        for s in buf {
                            *s *= gain;
                        }
                    }
                }
            }
        }

        Ok(ProcessStatus::Continue)
    }
}

// ============================================================================
// Plugin marker + factory
// ============================================================================

pub struct SuperDuperDsp;

impl Plugin for SuperDuperDsp {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(
        builder: &mut PluginExtensions<Self>,
        _shared: Option<&Self::Shared<'_>>,
    ) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>();
    }
}

impl DefaultPluginFactory for SuperDuperDsp {
    fn get_descriptor() -> PluginDescriptor {
        use clack_common::plugin::features::{AUDIO_EFFECT, STEREO, UTILITY};
        PluginDescriptor::new(PLUGIN_ID, PLUGIN_NAME)
            .with_vendor(PLUGIN_VENDOR)
            .with_version(PLUGIN_VERSION)
            .with_description("AI-authored DSP via Claude Code — M0 gain stub")
            .with_features([AUDIO_EFFECT, UTILITY, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        let s = PluginShared::new();
        dlog!("new_shared: instance {} ready", s.instance_id);
        Ok(s)
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        shared: &'a PluginShared,
    ) -> Result<PluginMainThread<'a>, PluginError> {
        Ok(PluginMainThread { shared })
    }
}

// ============================================================================
// Params extension — main thread side
// ============================================================================

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 {
        1
    }

    fn get_info(&mut self, param_index: u32, info: &mut ParamInfoWriter) {
        if param_index == 0 {
            info.set(&ParamInfo {
                id: gain_clap_id(),
                flags: ParamInfoFlags::IS_AUTOMATABLE,
                cookie: Default::default(),
                name: b"Gain",
                module: b"",
                min_value: PARAM_GAIN_MIN_DB,
                max_value: PARAM_GAIN_MAX_DB,
                default_value: PARAM_GAIN_DEFAULT_DB,
            });
        }
    }

    fn get_value(&mut self, param_id: ClapId) -> Option<f64> {
        if param_id == gain_clap_id() {
            Some(self.shared.gain_db.load(Ordering::Relaxed) as f64)
        } else {
            None
        }
    }

    fn value_to_text(
        &mut self,
        param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        if param_id == gain_clap_id() {
            use core::fmt::Write;
            write!(writer, "{:+.1} dB", value)?;
        }
        Ok(())
    }

    fn text_to_value(&mut self, param_id: ClapId, text: &CStr) -> Option<f64> {
        if param_id != gain_clap_id() {
            return None;
        }
        let s = text.to_str().ok()?.trim();
        let s = s.strip_suffix("dB").unwrap_or(s).trim();
        s.parse::<f64>()
            .ok()
            .map(|v| v.clamp(PARAM_GAIN_MIN_DB, PARAM_GAIN_MAX_DB))
    }

    fn flush(&mut self, input_events: &InputEvents, _output_events: &mut OutputEvents) {
        // Called when audio thread is inactive (offline param changes).
        dlog!("MainThread::flush: {} events", input_events.len());
        apply_param_events(self.shared, input_events);
    }
}

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 {
        // One stereo port on each side.
        1
    }

    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index != 0 {
            return;
        }
        let name: &[u8] = if is_input { b"Input" } else { b"Output" };
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name,
            channel_count: 2,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            // Paired with the same-id port on the opposite side — enables
            // in-place processing optimisation in the host.
            in_place_pair: Some(ClapId::new(0)),
        });
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, input_events: &InputEvents, _output_events: &mut OutputEvents) {
        dlog!("AudioProcessor::flush: {} events", input_events.len());
        apply_param_events(self.shared, input_events);
    }
}

// ============================================================================
// CLAP entry point
// ============================================================================

clack_export_entry!(SinglePluginEntry<SuperDuperDsp>);

// ============================================================================
// Unit tests (pure-Rust, no CLAP host)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_default_gain_is_unity() {
        let s = PluginShared::new();
        let g = s.gain_linear();
        assert!((g - 1.0).abs() < 1e-6);
    }

    #[test]
    fn plus_6db_doubles() {
        let s = PluginShared::new();
        s.gain_db.store(6.0, Ordering::Relaxed);
        let g = s.gain_linear();
        assert!((g - 1.995).abs() < 0.01, "+6 dB ≈ 2.0, got {g}");
    }

    #[test]
    fn minus_24db_attenuates() {
        let s = PluginShared::new();
        s.gain_db.store(-24.0, Ordering::Relaxed);
        let g = s.gain_linear();
        assert!(g < 0.07 && g > 0.06, "-24 dB ≈ 0.063, got {g}");
    }

    #[test]
    fn gain_id_is_zero() {
        assert_eq!(gain_clap_id().get(), 0);
    }
}
