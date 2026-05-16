//! SuperDuper DSP — CLAP plugin.
//!
//! M0: Hello CLAP + one `Gain` parameter.
//! M1: Native dylib hot-reload via [`HotReloadSlot`] + file watcher.
//!     Signal flow: input → effect.process() (if loaded) → gain → output.
//!     Effect panics are caught, instance is poisoned, audio keeps flowing.

#![allow(clippy::missing_safety_doc)]

mod build_pipeline;
mod hotreload;
mod mcp_registry;
mod mcp_server;
mod watcher;

pub use hotreload::{HotReloadSlot, ProcessFn, SDSP_PROTOCOL_VERSION, SwapError};

use atomic_float::AtomicF32;
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::params::{
    HostParams, ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, ParamRescanFlags,
    PluginAudioProcessorParams, PluginMainThreadParams, PluginParams,
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

/// Maximum effect parameters supported. Effect IDs occupy 1..=MAX_EFFECT_PARAMS
/// (host Gain stays at id 0; Reload occupies MAX_EFFECT_PARAMS + 1).
pub const MAX_EFFECT_PARAMS: usize = 32;

/// "Reload effect dylib" toggle. Flipping this from 0 → 1 forces the plugin
/// to re-`slot.swap()` from the current `effect_dylib_path`, even if the file
/// mtime didn't change (e.g. when the watcher didn't fire). Useful for both
/// debug and when running without a working file watcher.
pub const PARAM_RELOAD_ID: u32 = (MAX_EFFECT_PARAMS as u32) + 1;

const fn gain_clap_id() -> ClapId {
    // 0 is a valid ClapId — the type only forbids `u32::MAX`.
    ClapId::new(PARAM_GAIN_ID)
}

const fn reload_clap_id() -> ClapId {
    ClapId::new(PARAM_RELOAD_ID)
}

/// `id == 0` → host Gain, `id 1..=MAX_EFFECT_PARAMS` → effect-defined param.
/// Returns the effect param index (0-based) for ids in that range.
fn effect_param_index(id: ClapId) -> Option<usize> {
    let raw = id.get();
    if raw == 0 || raw > MAX_EFFECT_PARAMS as u32 {
        None
    } else {
        Some((raw - 1) as usize)
    }
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
    /// Per-effect parameter values, indexed by id-1 (id 0 is host Gain).
    /// Written by the host via `ParamValueEvent`, read by the audio thread
    /// via snapshot into a stack array each `process()` call.
    pub effect_params: [AtomicF32; MAX_EFFECT_PARAMS],
    /// Set when a CLAP Reload event arrives; the main thread observes it and
    /// re-`slot.swap()`s from `effect_dylib_path`. Clear-on-handle.
    pub reload_requested: AtomicBool,
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

        // Eager-load: if a dylib already lives at the per-instance path (left
        // over from a prior session, or dropped in by `scripts/load_effect.sh`
        // before REAPER finished loading the plugin), swap it in synchronously
        // now. `slot.swap` itself is fast (dlopen + a couple of dlsym calls
        // + a Vec<EffectParam> alloc), so it doesn't trip the scan-timeout
        // problem that the FSEvents watcher init does.
        if effect_dylib_path.exists() {
            match slot.swap(&effect_dylib_path) {
                Ok(()) => {
                    dlog!("eager swap loaded {:?}", effect_dylib_path);
                }
                Err(e) => {
                    dlog!("eager swap of {:?} failed: {}", effect_dylib_path, e);
                }
            }
        }

        // NOTE: watcher init is still deferred to `ensure_watcher()` called
        // from `activate()`. `notify::recommended_watcher` can block briefly
        // while initialising FSEvents on macOS, and during CLAP plugin scan
        // that latency made REAPER's main thread unresponsive to subsequent
        // API calls. By deferring, scan stays cheap.
        Self {
            gain_db: AtomicF32::new(PARAM_GAIN_DEFAULT_DB as f32),
            bypass: AtomicBool::new(false),
            slot,
            instance_id,
            effect_dylib_path,
            effect_params: std::array::from_fn(|_| AtomicF32::new(0.0)),
            reload_requested: AtomicBool::new(false),
            process_calls: AtomicU64::new(0),
            events_seen: AtomicU64::new(0),
            _watcher: parking_lot::Mutex::new(None),
        }
    }

    /// Lazy-init watcher on first audio activation. Idempotent.
    pub fn ensure_watcher(&self) {
        // Register as MCP primary now that we have a stable host-owned address
        // for this PluginShared. `new_shared` saw a stack-local copy that got
        // moved on return — registering its address there gave the registry
        // a dangling pointer.
        mcp_registry::register_first(self);

        let mut guard = self._watcher.lock();
        if guard.is_some() {
            return;
        }
        dlog!("ensure_watcher: starting notify watcher");

        // Catch any dylib that landed between `new_shared` and `activate()`.
        if self.effect_dylib_path.exists() && !self.slot.is_loaded() {
            match self.slot.swap(&self.effect_dylib_path) {
                Ok(()) => dlog!("activate-time swap loaded {:?}", self.effect_dylib_path),
                Err(e) => dlog!("activate-time swap failed: {}", e),
            }
        }

        match watcher::start(self.slot.clone(), self.effect_dylib_path.clone()) {
            Ok(handle) => {
                *guard = Some(handle);
                dlog!("watcher started");
            }
            Err(e) => dlog!("file watcher did not start ({}); hot-reload disabled", e),
        }
    }

    // ensure_mcp moved into the `mcp_registry` module which manages a
    // process-global server bound to the primary instance — see that module
    // for the safety rationale.

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
    host: HostMainThreadHandle<'a>,
}

impl<'a> PluginMainThread<'a> {
    /// If a fresh swap landed since the last call, ask the host to re-enumerate
    /// our parameters. Cheap no-op when the flag isn't set.
    fn maybe_rescan(&mut self) {
        if !self
            .shared
            .slot
            .rescan_needed
            .swap(false, Ordering::AcqRel)
        {
            return;
        }
        let Some(host_params) = self.host.shared().get_extension::<HostParams>() else {
            return;
        };
        host_params.rescan(&mut self.host, ParamRescanFlags::ALL);
        dlog!("requested host params.rescan(ALL)");
    }

    /// If the user toggled the Reload param since last check, force-swap the
    /// current effect dylib (mtime change not required). Lets the user
    /// recover when the file watcher didn't fire.
    fn maybe_reload(&mut self) {
        if !self
            .shared
            .reload_requested
            .swap(false, Ordering::AcqRel)
        {
            return;
        }
        let path = &self.shared.effect_dylib_path;
        if !path.exists() {
            dlog!("Reload pressed but {:?} doesn't exist; nothing to do", path);
            return;
        }
        match self.shared.slot.swap(path) {
            Ok(()) => dlog!("manual reload swap OK from {:?}", path),
            Err(e) => dlog!("manual reload swap failed: {}", e),
        }
    }
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {
    fn on_main_thread(&mut self) {
        // Audio thread asked us (via host.request_callback) to do main-thread
        // work — namely: pending reload swap or param rescan.
        self.maybe_reload();
        self.maybe_rescan();
    }
}

// ============================================================================
// Audio processor state
// ============================================================================

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    host: HostAudioProcessorHandle<'a>,
}

/// Apply param_value events to `shared`. Routes by `param_id`:
/// - id 0 → host Gain (`gain_db`)
/// - id 1..=MAX_EFFECT_PARAMS → effect param at index `id - 1`
///
/// Used by both the audio processor (process + flush) and the main thread (flush).
fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    for event in events {
        let Some(pv) = event.as_event::<ParamValueEvent>() else {
            continue;
        };
        let Some(id) = pv.param_id() else { continue };
        let value = pv.value() as f32;
        if id == gain_clap_id() {
            shared.gain_db.store(value, Ordering::Relaxed);
            let count = shared.events_seen.fetch_add(1, Ordering::Relaxed) + 1;
            dlog!("ParamValueEvent #{}: gain → {:+.2} dB", count, value);
        } else if id == reload_clap_id() {
            // Treat any non-zero value as "user pressed the button".
            // Main thread will pick up the flag and call slot.swap().
            if value > 0.5 {
                shared.reload_requested.store(true, Ordering::Release);
                dlog!("ParamValueEvent: Reload pressed");
            }
        } else if let Some(idx) = effect_param_index(id) {
            shared.effect_params[idx].store(value, Ordering::Relaxed);
            dlog!("ParamValueEvent: effect[{}] → {:+.4}", idx, value);
        }
    }
}

/// Reset every effect param atomic to the loaded effect's declared default.
/// Called from non-audio threads after a fresh swap; cheap atomic stores.
fn sync_effect_param_defaults(shared: &PluginShared) {
    let meta = shared.slot.meta();
    for (idx, p) in meta.params.iter().enumerate().take(MAX_EFFECT_PARAMS) {
        shared.effect_params[idx].store(p.default, Ordering::Relaxed);
    }
}

impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, PluginShared, PluginMainThread<'a>>
    for PluginAudioProcessor<'a>
{
    fn activate(
        host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut PluginMainThread<'a>,
        shared: &'a PluginShared,
        _audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        // Lazy-start the file watcher now that the plugin is actually being
        // used (not just scanned). Idempotent. We don't do this in
        // `new_shared` because notify init can block briefly and would stall
        // REAPER's CLAP scan path.
        shared.ensure_watcher();
        Ok(Self { shared, host })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // After a fresh swap, sync defaults from the new metadata before we
        // read effect_params for this block. Cheap: just N atomic stores.
        if self.shared.slot.rescan_needed.load(Ordering::Acquire) {
            sync_effect_param_defaults(self.shared);
            // We don't clear `rescan_needed` here — that's the main thread's
            // job once it's done re-querying params. Atomic stores above are
            // idempotent, so a few redundant syncs are fine.
        }

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

        // If a Reload event just landed (or a watcher swap set rescan_needed),
        // poke the host to schedule on_main_thread() ASAP. We can't do dlopen
        // on the audio thread.
        if self.shared.reload_requested.load(Ordering::Acquire)
            || self.shared.slot.rescan_needed.load(Ordering::Acquire)
        {
            self.host.shared().request_callback();
        }

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

        // Atomic snapshot of effect params into a stack array. Audio thread
        // sees a coherent set within this block — no half-old/half-new reads
        // mid-callback while automation crank-races us.
        let mut params_snap = [0.0_f32; MAX_EFFECT_PARAMS];
        for (i, atom) in self.shared.effect_params.iter().enumerate() {
            params_snap[i] = atom.load(Ordering::Relaxed);
        }
        let params_ptr = params_snap.as_ptr();

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
                                params_ptr,
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
        // NB: do NOT register the MCP primary here — `s` lives on the stack
        // and is `Ok(s)`-moved out of this fn, so its address changes.
        // Deferred to `ensure_watcher()` which sees `&self` at the final
        // host-owned address.
        Ok(s)
    }

    fn new_main_thread<'a>(
        host: HostMainThreadHandle<'a>,
        shared: &'a PluginShared,
    ) -> Result<PluginMainThread<'a>, PluginError> {
        Ok(PluginMainThread { shared, host })
    }
}

// ============================================================================
// Params extension — main thread side
// ============================================================================

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 {
        // Trigger rescan request on first main-thread interaction after a swap.
        self.maybe_rescan();
        // Manual Reload toggle handler (force re-swap from effect_dylib_path).
        self.maybe_reload();
        let effect_count = self.shared.slot.meta().params.len() as u32;
        // Cap at MAX_EFFECT_PARAMS — anything past that is a bug in the effect.
        let effect_count = effect_count.min(MAX_EFFECT_PARAMS as u32);
        // 1 (Gain) + N (effect) + 1 (Reload toggle)
        1 + effect_count + 1
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
            return;
        }
        let meta = self.shared.slot.meta();
        let effect_count = meta.params.len().min(MAX_EFFECT_PARAMS) as u32;
        if param_index <= effect_count {
            // Effect params: ids 1..=N. param_index is 1-based here.
            let idx = (param_index - 1) as usize;
            if let Some(p) = meta.params.get(idx) {
                info.set(&ParamInfo {
                    id: ClapId::new(param_index),
                    flags: ParamInfoFlags::IS_AUTOMATABLE,
                    cookie: Default::default(),
                    name: p.name.as_bytes(),
                    module: b"Effect",
                    min_value: p.min as f64,
                    max_value: p.max as f64,
                    default_value: p.default as f64,
                });
            }
            return;
        }
        // Last slot is the Reload toggle.
        if param_index == effect_count + 1 {
            info.set(&ParamInfo {
                id: reload_clap_id(),
                flags: ParamInfoFlags::IS_AUTOMATABLE | ParamInfoFlags::IS_STEPPED,
                cookie: Default::default(),
                name: b"Reload",
                module: b"",
                min_value: 0.0,
                max_value: 1.0,
                default_value: 0.0,
            });
        }
    }

    fn get_value(&mut self, param_id: ClapId) -> Option<f64> {
        if param_id == gain_clap_id() {
            return Some(self.shared.gain_db.load(Ordering::Relaxed) as f64);
        }
        if param_id == reload_clap_id() {
            // Always reports 0 — the param is a momentary trigger, not state.
            return Some(0.0);
        }
        let idx = effect_param_index(param_id)?;
        Some(self.shared.effect_params[idx].load(Ordering::Relaxed) as f64)
    }

    fn value_to_text(
        &mut self,
        param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        use core::fmt::Write;
        if param_id == gain_clap_id() {
            return write!(writer, "{:+.1} dB", value);
        }
        if param_id == reload_clap_id() {
            return write!(writer, "{}", if value > 0.5 { "RELOAD" } else { "idle" });
        }
        let Some(idx) = effect_param_index(param_id) else {
            return Ok(());
        };
        let meta = self.shared.slot.meta();
        let unit = meta.params.get(idx).map(|p| p.unit.as_str()).unwrap_or("");
        if unit.is_empty() {
            write!(writer, "{:.3}", value)
        } else {
            write!(writer, "{:.3} {}", value, unit)
        }
    }

    fn text_to_value(&mut self, param_id: ClapId, text: &CStr) -> Option<f64> {
        let s = text.to_str().ok()?.trim();
        if param_id == gain_clap_id() {
            let s = s.strip_suffix("dB").unwrap_or(s).trim();
            return s
                .parse::<f64>()
                .ok()
                .map(|v| v.clamp(PARAM_GAIN_MIN_DB, PARAM_GAIN_MAX_DB));
        }
        let idx = effect_param_index(param_id)?;
        let meta = self.shared.slot.meta();
        let p = meta.params.get(idx)?;
        // Strip the unit suffix if present.
        let s = if !p.unit.is_empty() {
            s.strip_suffix(&p.unit).unwrap_or(s).trim()
        } else {
            s
        };
        s.parse::<f64>()
            .ok()
            .map(|v| v.clamp(p.min as f64, p.max as f64))
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
