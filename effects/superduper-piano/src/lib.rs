//! SuperDuper Piano — SFZ multisample player. Built for pianos (pedal,
//! release samples, velocity layers) but plays any SFZ pack.
//!
//! Packs are folders under `~/Music/SFZ/`: drop one in, press Rescan, it is
//! in the list. Nothing is bundled — the default is Salamander Grand V3 if it
//! is there, otherwise the first pack found.
//!
//! Threads: a loader thread (spawned in `new_shared`, idle until the plugin is
//! first activated so a host's plugin scan never decodes a gigabyte) parses +
//! decodes and hands the finished `Arc<Instrument>` over through `pending`.
//! The audio thread swaps it in with `try_lock` and passes the old one back
//! through `retired`, so the drop — which frees the samples — happens on the
//! loader thread, never on the audio thread.

#![allow(clippy::missing_safety_doc)]

pub mod gui;

use atomic_float::AtomicF32;
use clack_common::events::Match;
use clack_common::stream::{InputStream, OutputStream};
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams,
    PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use parking_lot::Mutex;
use std::ffi::CStr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU8, Ordering};
use std::sync::Arc;
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::Tilt;
use superduper_synth_core::sfz::load::{default_pack_root, load_instrument, scan_packs, LoadProgress, PackEntry};
use superduper_synth_core::sfz::{Controls, Engine, Instrument, DEFAULT_VOICES};

fn init_logging() { superduper_dsp_sdk::log::init("piano"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Param table — frozen once shipped (REAPER caches it per slot).
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    // Index into the scanned pack list (sorted by pack, then name). 1023 so a
    // big SFZ folder never hits the ceiling the Sampler's 255 did.
    ParamDef { id: 0, name: b"Instrument", min: 0.0,   max: 1023.0, default: 0.0,  unit: ""   },
    ParamDef { id: 1, name: b"Output",     min: -36.0, max: 12.0,   default: -6.0, unit: "dB" },
    // Whole semitones shift which KEY is played, so each note still uses its
    // own recorded sample rather than a neighbour stretched.
    ParamDef { id: 2, name: b"Transpose",  min: -24.0, max: 24.0,   default: 0.0,  unit: "st" },
    ParamDef { id: 3, name: b"Tune",       min: -100.0, max: 100.0, default: 0.0,  unit: "ct" },
    // −1 hard touch, 0 as played, +1 soft touch.
    ParamDef { id: 4, name: b"Vel Curve",  min: -1.0,  max: 1.0,    default: 0.0,  unit: ""   },
    ParamDef { id: 5, name: b"Release",    min: 0.25,  max: 4.0,    default: 1.0,  unit: "x"  },
    ParamDef { id: 6, name: b"Tone",       min: -6.0,  max: 6.0,    default: 0.0,  unit: "dB" },
    ParamDef { id: 7, name: b"Width",      min: 0.0,   max: 1.5,    default: 1.0,  unit: ""   },
    // Latches the sustain pedal — for playing from a mouse, a pad or an agent.
    ParamDef { id: 8, name: b"Hold",       min: 0.0,   max: 1.0,    default: 0.0,  unit: ""   },
];

pub const P_INSTRUMENT: usize = 0;
pub const P_OUTPUT: usize = 1;
pub const P_TRANSPOSE: usize = 2;
pub const P_TUNE: usize = 3;
pub const P_VEL_CURVE: usize = 4;
pub const P_RELEASE: usize = 5;
pub const P_TONE: usize = 6;
pub const P_WIDTH: usize = 7;
pub const P_HOLD: usize = 8;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// What the GUI shows about the loaded instrument.
#[derive(Debug, Clone, Default)]
pub struct InstrumentInfo {
    pub label: String,
    pub path: Option<PathBuf>,
    pub regions: usize,
    pub samples: usize,
    pub megabytes: f64,
    pub load_seconds: f64,
    pub missing: Vec<String>,
    pub unsupported: Vec<String>,
    /// Lowest / highest key any note region answers to (keyswitches excluded).
    pub key_range: Option<(u8, u8)>,
}

pub type SharedParams = Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub dirty_params: [AtomicBool; PARAMS.len()],
    pub gesture_begin: [AtomicBool; PARAMS.len()],
    pub gesture_end: [AtomicBool; PARAMS.len()],
    pub pack_root: PathBuf,
    pub library: Mutex<Vec<PackEntry>>,
    /// Library index of what is loaded (or being loaded). −1 = nothing yet.
    pub current_index: AtomicI32,
    /// The `Instrument` value the loader last acted on. The loader never
    /// writes the param (CLAP: the host owns it; writing the preferred
    /// default back made flush() and process() read differently —
    /// clap-validator param-set-events, flaky, 2026-10-21); a fresh instance
    /// loads the preferred instrument while the param stays at its default,
    /// and a host move away from the last seen value loads that entry.
    pub param_seen: AtomicI32,
    /// A specific file to load next (GUI pick, project state). Wins over the
    /// Instrument param.
    pub requested_path: Mutex<Option<PathBuf>>,
    /// What a project should recall: the last path asked for, whether or not
    /// the loader has finished with it. `save()` read `requested_path`, which
    /// the loader `take()`s, then `info.path`, which it fills only once
    /// loaded — in between, a save wrote `null` (clap-validator
    /// state-buffered-streams, 1 run in 3, 2026-10-21).
    pub state_path: Mutex<Option<PathBuf>>,
    pub info: Mutex<InstrumentInfo>,
    pub status: Mutex<String>,
    pub progress: LoadProgress,
    pub loading: AtomicBool,
    /// Set on first activate — the loader does nothing before it.
    pub activated: AtomicBool,
    pub pending: Mutex<Option<Arc<Instrument>>>,
    pub retired: Mutex<Vec<Arc<Instrument>>>,
    pub active_voices: AtomicU32,
    pub host_sr: AtomicF32,
    /// GUI audition: a MIDI key, or −1.
    pub audition_request: AtomicI32,
    /// Per-key display state from the audio thread: 2 held, 1 sounding.
    pub key_state: [AtomicU8; 128],
    pub pedal_on: AtomicBool,
    /// Key the GUI keyboard is holding down (−1 = none) and its velocity.
    pub gui_key: AtomicI32,
    pub gui_velocity: AtomicF32,
}

pub struct PluginShared { pub inner: SharedParams }

impl PluginShared {
    /// `SUPERDUPER_PIANO_ROOT` overrides `~/Music/SFZ` — tests point it at a
    /// fixture pack so they never depend on what this machine has installed.
    pub fn new() -> Self {
        Self::with_root(std::env::var_os("SUPERDUPER_PIANO_ROOT").map(PathBuf::from).unwrap_or_else(default_pack_root))
    }

    pub fn with_root(pack_root: PathBuf) -> Self {
        Self {
            inner: Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: AtomicBool::new(false),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(4096),
                dirty_params: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| AtomicBool::new(false)),
                pack_root,
                library: Mutex::new(Vec::new()),
                current_index: AtomicI32::new(-1),
                param_seen: AtomicI32::new(PARAMS[P_INSTRUMENT].default as i32),
                requested_path: Mutex::new(None),
                state_path: Mutex::new(None),
                info: Mutex::new(InstrumentInfo::default()),
                status: Mutex::new(String::new()),
                progress: LoadProgress::default(),
                loading: AtomicBool::new(false),
                activated: AtomicBool::new(false),
                pending: Mutex::new(None),
                retired: Mutex::new(Vec::with_capacity(RETIRE_CAPACITY)),
                active_voices: AtomicU32::new(0),
                host_sr: AtomicF32::new(48_000.0),
                audition_request: AtomicI32::new(-1),
                key_state: std::array::from_fn(|_| AtomicU8::new(0)),
                pedal_on: AtomicBool::new(false),
                gui_key: AtomicI32::new(-1),
                gui_velocity: AtomicF32::new(0.7),
            }),
        }
    }

    pub fn shared_handle(&self) -> SharedParams { Arc::clone(&self.inner) }
}

impl Default for PluginShared { fn default() -> Self { Self::new() } }
impl std::ops::Deref for PluginShared {
    type Target = SharedParamsInner;
    fn deref(&self) -> &SharedParamsInner { &self.inner }
}
impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

/// Successful loads in this process — lets a test wait for the loader.
pub static LOADS_COMPLETED: AtomicU32 = AtomicU32::new(0);

/// The audio thread pushes into `retired` only within this capacity, so the
/// push never allocates.
const RETIRE_CAPACITY: usize = 8;

/// Rescan the pack folder. Keeps `current_index` pointing at the loaded file
/// when it is still there. Returns the entry count.
pub fn refresh_library(shared: &SharedParamsInner) -> usize {
    let entries = scan_packs(&shared.pack_root);
    let loaded = shared.info.lock().path.clone();
    if let Some(p) = loaded {
        if let Some(i) = entries.iter().position(|e| e.path == p) {
            shared.current_index.store(i as i32, Ordering::Relaxed);
        }
    }
    let n = entries.len();
    *shared.library.lock() = entries;
    n
}

/// GUI pick: load the i-th library entry and record it for automation.
pub fn pick_instrument(shared: &SharedParamsInner, idx: usize) {
    let path = shared.library.lock().get(idx).map(|e| e.path.clone());
    if let Some(p) = path {
        shared.params[P_INSTRUMENT].store(idx as f32, Ordering::Relaxed);
        shared.dirty_params[P_INSTRUMENT].store(true, Ordering::Relaxed);
        shared.param_seen.store(idx as i32, Ordering::Relaxed);
        request_path(shared, p);
    }
}

/// Ask the loader for `p` and remember it for the project state.
pub fn request_path(shared: &SharedParamsInner, p: PathBuf) {
    *shared.state_path.lock() = Some(p.clone());
    *shared.requested_path.lock() = Some(p);
}

/// The pack a fresh instance starts on: Salamander when present.
fn preferred_default(lib: &[PackEntry]) -> Option<usize> {
    if lib.is_empty() { return None; }
    Some(lib.iter().position(|e| e.name.to_lowercase().contains("salamander")).unwrap_or(0))
}

/// One loader step: decide what should be loaded and load it. Blocking — runs
/// on the loader thread (and in tests).
pub fn loader_step(shared: &SharedParamsInner) {
    // Free what the audio thread retired.
    shared.retired.lock().clear();
    if !shared.activated.load(Ordering::Relaxed) { return; }

    let lib = shared.library.lock().clone();
    let loaded = shared.info.lock().path.clone();
    let want_idx = shared.params[P_INSTRUMENT].load(Ordering::Relaxed).round() as i32;
    let current = shared.current_index.load(Ordering::Relaxed);
    let seen = shared.param_seen.load(Ordering::Relaxed);
    let target: Option<PathBuf> = if let Some(p) = shared.requested_path.lock().take() {
        Some(p)
    } else if want_idx != seen {
        // The host moved the param: that entry, whatever is loaded.
        shared.param_seen.store(want_idx, Ordering::Relaxed);
        lib.get(want_idx.max(0) as usize).map(|e| e.path.clone())
    } else if current == -1 && loaded.is_none() {
        // Fresh instance, no project state: the preferred default, not
        // whatever happens to sort first. The param is left alone.
        preferred_default(&lib).map(|i| lib[i].path.clone())
    } else {
        None
    };
    let Some(target) = target else { return };
    let idx = lib.iter().position(|e| e.path == target);
    if loaded.as_ref() == Some(&target) {
        shared.current_index.store(idx.map_or(current, |i| i as i32), Ordering::Relaxed);
        return;
    }

    // Claim the index before loading so a slow load is not restarted.
    shared.current_index.store(idx.map_or(-2, |i| i as i32), Ordering::Relaxed);
    let label = idx
        .and_then(|i| lib.get(i))
        .map(|e| format!("{} / {}", e.pack, e.name))
        .unwrap_or_else(|| target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    *shared.status.lock() = format!("loading {label}…");
    shared.loading.store(true, Ordering::Relaxed);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let t0 = std::time::Instant::now();
    let result = load_instrument(&target, threads, Some(&shared.progress));
    shared.loading.store(false, Ordering::Relaxed);
    match result {
        Ok(inst) => {
            let info = InstrumentInfo {
                label: label.clone(),
                path: Some(target.clone()),
                regions: inst.def.regions.len(),
                samples: inst.samples.len(),
                megabytes: inst.samples.iter().map(|s| s.data.len() * 2).sum::<usize>() as f64 / 1e6,
                load_seconds: t0.elapsed().as_secs_f64(),
                missing: inst.missing.clone(),
                unsupported: inst.def.unsupported.clone(),
                key_range: {
                    let notes = inst.def.regions.iter().filter(|r| r.on_cc.is_none() && r.lokey >= 0 && r.hikey >= r.lokey);
                    let lo = notes.clone().map(|r| r.lokey).min();
                    let hi = notes.map(|r| r.hikey).max();
                    lo.zip(hi).map(|(a, b)| (a.clamp(0, 127) as u8, b.clamp(0, 127) as u8))
                },
            };
            slog!("loaded {} — {} regions, {:.0} MB in {:.1}s, {} missing", label, info.regions, info.megabytes, info.load_seconds, info.missing.len());
            *shared.status.lock() = format!("{label}: {} regions, {:.0} MB, {:.1} s", info.regions, info.megabytes, info.load_seconds);
            *shared.info.lock() = info;
            // A previous load the audio thread never picked up is dropped
            // here, on this thread.
            let stale = shared.pending.lock().replace(Arc::new(inst));
            drop(stale);
            LOADS_COMPLETED.fetch_add(1, Ordering::SeqCst);
        }
        Err(e) => {
            slog!("load {} failed: {}", target.display(), e);
            *shared.status.lock() = format!("{label}: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Main thread + audio processor
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    track_watch: superduper_synth_core::gui::TrackInfoWatch<'a>,
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    engine: Engine,
    inst: Arc<Instrument>,
    /// An old instrument the `retired` list had no room (or lock) for yet.
    retire_slot: Option<Arc<Instrument>>,
    sample_rate: f32,
    /// Played key → engine key, so a note-off after a Transpose change still
    /// finds the note it started.
    key_map: [u8; 128],
    midi_pedal: f32,
    pedal_sent: f32,
    tilt_l: Tilt,
    tilt_r: Tilt,
    audition_key: Option<u8>,
    audition_blocks: u32,
    /// The key the GUI keyboard is sounding, so a release or a drag to
    /// another key sends the matching note-off.
    gui_playing: Option<u8>,
    activity: [u8; 128],
}

impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, PluginShared, PluginMainThread<'a>>
    for PluginAudioProcessor<'a>
{
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut PluginMainThread<'a>,
        shared: &'a PluginShared,
        cfg: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        shared.scope.set_sample_rate(cfg.sample_rate as f32);
        init_logging();
        let sr = cfg.sample_rate as f32;
        shared.host_sr.store(sr, Ordering::Relaxed);
        shared.activated.store(true, Ordering::Relaxed);
        // Re-activation (sample-rate change) must not lose the instrument:
        // if one is loaded but already consumed by a previous processor,
        // ask the loader for it again.
        if shared.pending.lock().is_none() {
            if let Some(p) = shared.info.lock().path.clone() {
                shared.info.lock().path = None;
                request_path(shared, p);
            }
        }
        slog!("activate sr={}", sr);
        Ok(Self {
            shared,
            engine: Engine::new(sr, DEFAULT_VOICES),
            inst: Arc::new(Instrument::default()),
            retire_slot: None,
            sample_rate: sr,
            key_map: std::array::from_fn(|i| i as u8),
            midi_pedal: 0.0,
            pedal_sent: -1.0,
            tilt_l: Tilt::default(),
            tilt_r: Tilt::default(),
            audition_key: None,
            audition_blocks: 0,
            gui_playing: None,
            activity: [0; 128],
        })
    }

    fn process(&mut self, _process: Process, mut audio: Audio, events: Events) -> Result<ProcessStatus, PluginError> {
        let _denormals = superduper_dsp_sdk::denormals::Guard::new();
        let shared = self.shared;
        superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events.input);
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(&shared.params, &shared.dirty_params, events.output);
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(&shared.gesture_begin, &shared.gesture_end, events.output);

        self.swap_in_pending();
        let p = |i: usize| shared.params[i].load(Ordering::Relaxed);
        self.engine.controls = Controls {
            tune_cents: p(P_TUNE),
            release_scale: p(P_RELEASE),
            vel_curve: p(P_VEL_CURVE),
        };
        self.sync_pedal(p(P_HOLD));

        let req = shared.audition_request.swap(-1, Ordering::AcqRel);
        if (0..128).contains(&req) {
            if let Some(k) = self.audition_key.take() { self.engine.note_off(&self.inst, k); }
            self.engine.note_on(&self.inst, req as u8, 0.7);
            self.audition_key = Some(req as u8);
            self.audition_blocks = (self.sample_rate * 0.8 / 256.0) as u32;
        } else if let Some(k) = self.audition_key {
            self.audition_blocks = self.audition_blocks.saturating_sub(1);
            if self.audition_blocks == 0 {
                self.engine.note_off(&self.inst, k);
                self.audition_key = None;
            }
        }

        let want = shared.gui_key.load(Ordering::Relaxed);
        let want = (0..128).contains(&want).then_some(want as u8);
        if want != self.gui_playing {
            if let Some(k) = self.gui_playing.take() { self.engine.note_off(&self.inst, k); }
            if let Some(k) = want {
                self.engine.note_on(&self.inst, k, shared.gui_velocity.load(Ordering::Relaxed));
                self.gui_playing = Some(k);
            }
        }

        let bypassed = shared.bypass.load(Ordering::Relaxed);
        let out_gain = 10f32.powf(p(P_OUTPUT) / 20.0);
        let tilt = (p(P_TONE) / 6.0).clamp(-1.0, 1.0);
        let width = p(P_WIDTH);
        let transpose = p(P_TRANSPOSE).round() as i32;
        let sr = self.sample_rate;

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            let mut writers = channel_pairs.into_iter().filter_map(superduper_dsp_sdk::clap_helpers::output_slice);
            let (Some(out_l), Some(out_r)) = (writers.next(), writers.next()) else { continue };
            let frames = out_l.len().min(out_r.len());
            out_l[..frames].fill(0.0);
            out_r[..frames].fill(0.0);

            for batch in events.input.batch() {
                for ev in batch.events() {
                    if let Some(core) = ev.as_core_event() {
                        self.handle_event(core, transpose);
                    }
                }
                let start = batch.first_sample().min(frames);
                let end = batch.next_batch_first_sample().unwrap_or(frames).min(frames);
                if end > start && !bypassed {
                    self.engine.render(&self.inst, &mut out_l[start..end], &mut out_r[start..end]);
                }
            }

            for i in 0..frames {
                let (mut l, mut r) = (out_l[i], out_r[i]);
                if tilt.abs() > 1e-4 {
                    l = self.tilt_l.process(l, sr, tilt);
                    r = self.tilt_r.process(r, sr, tilt);
                }
                if (width - 1.0).abs() > 1e-4 {
                    let m = 0.5 * (l + r);
                    let s = 0.5 * (l - r) * width;
                    l = m + s;
                    r = m - s;
                }
                out_l[i] = l * out_gain;
                out_r[i] = r * out_gain;
                shared.scope.push(0.5 * (out_l[i] + out_r[i]));
            }
        }
        shared.active_voices.store(self.engine.active_voices() as u32, Ordering::Relaxed);
        self.engine.key_activity(&self.inst, &mut self.activity);
        for (slot, &v) in shared.key_state.iter().zip(self.activity.iter()) { slot.store(v, Ordering::Relaxed); }
        shared.pedal_on.store(self.engine.pedal_is_down(), Ordering::Relaxed);
        Ok(ProcessStatus::Continue)
    }
}

impl PluginAudioProcessor<'_> {
    fn swap_in_pending(&mut self) {
        if let Some(old) = self.retire_slot.take() { self.retire(old); }
        let fresh = self.shared.pending.try_lock().and_then(|mut g| g.take());
        if let Some(new) = fresh {
            self.engine.reset(&new);
            self.pedal_sent = -1.0;
            let old = std::mem::replace(&mut self.inst, new);
            self.retire(old);
        }
    }

    /// Hand an instrument back to the loader thread to drop. Never allocates:
    /// a full or locked list keeps it in `retire_slot` for the next block.
    fn retire(&mut self, old: Arc<Instrument>) {
        match self.shared.retired.try_lock() {
            Some(mut list) if list.len() < list.capacity() => list.push(old),
            _ => self.retire_slot = Some(old),
        }
    }

    fn sync_pedal(&mut self, hold: f32) {
        let eff = if hold >= 0.5 { 1.0 } else { self.midi_pedal };
        if (eff - self.pedal_sent).abs() > 1e-6 {
            self.engine.cc(&self.inst, 64, eff);
            self.pedal_sent = eff;
        }
    }

    fn note_on(&mut self, key: u8, vel: f32, transpose: i32) {
        let Some(slot) = self.key_map.get_mut(key as usize) else { return };
        let k = (key as i32 + transpose).clamp(0, 127) as u8;
        *slot = k;
        self.engine.note_on(&self.inst, k, vel);
    }

    fn note_off(&mut self, key: u8) {
        if let Some(&k) = self.key_map.get(key as usize) {
            self.engine.note_off(&self.inst, k);
        }
    }

    fn handle_event(&mut self, core: CoreEventSpace<'_>, transpose: i32) {
        match core {
            CoreEventSpace::NoteOn(n) => {
                if let Match::Specific(k) = n.key() {
                    self.note_on(k as u8, n.velocity().clamp(0.0, 1.0) as f32, transpose);
                }
            }
            CoreEventSpace::NoteOff(n) => {
                if let Match::Specific(k) = n.key() { self.note_off(k as u8); }
            }
            CoreEventSpace::NoteChoke(_) => self.engine.all_notes_off(),
            CoreEventSpace::Midi(m) => {
                let d = m.data();
                match d[0] & 0xF0 {
                    0x90 if d[2] > 0 => self.note_on(d[1], d[2] as f32 / 127.0, transpose),
                    0x90 | 0x80 => self.note_off(d[1]),
                    0xB0 => match d[1] {
                        64 => {
                            self.midi_pedal = d[2] as f32 / 127.0;
                            let hold = self.shared.params[P_HOLD].load(Ordering::Relaxed);
                            self.sync_pedal(hold);
                        }
                        120 | 123 => self.engine.all_notes_off(),
                        n => self.engine.cc(&self.inst, n, d[2] as f32 / 127.0),
                    },
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 { if is_input { 0 } else { 1 } }
    fn get(&mut self, index: u32, is_input: bool, w: &mut AudioPortInfoWriter) {
        if is_input || index != 0 { return; }
        w.set(&AudioPortInfo {
            id: ClapId::new(0), name: b"Output", channel_count: 2,
            flags: AudioPortFlags::IS_MAIN, port_type: Some(AudioPortType::STEREO),
            in_place_pair: None,
        });
    }
}

impl PluginNotePortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 { if is_input { 1 } else { 0 } }
    fn get(&mut self, index: u32, is_input: bool, w: &mut NotePortInfoWriter) {
        if !is_input || index != 0 { return; }
        w.set(&NotePortInfo {
            id: ClapId::new(0), name: b"MIDI In",
            supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
            preferred_dialect: Some(NoteDialect::Clap),
        });
    }
}

impl PluginMainThread<'_> {
    /// What a parameter value reads as — the one place for it, so the host's
    /// text entry (`text_to_value`) accepts exactly what the display shows.
    fn display_text<W: core::fmt::Write>(&mut self, id: ClapId, v: f64, w: &mut W) -> core::fmt::Result {
        match id.get() as usize {
            P_INSTRUMENT => {
                if let Some(e) = self.shared.library.lock().get(v.round().max(0.0) as usize) {
                    return write!(w, "{} / {}", e.pack, e.name);
                }
            }
            P_HOLD => return write!(w, "{}", if v >= 0.5 { "On" } else { "Off" }),
            _ => {}
        }
        ParamDef::write_display(PARAMS, id, v, w)
    }
}

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 { PARAMS.len() as u32 }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) { ParamDef::write_info(PARAMS, idx, info); }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        self.shared.params.get(id.get() as usize).map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(&mut self, id: ClapId, v: f64, w: &mut ParamDisplayWriter) -> core::fmt::Result {
        self.display_text(id, v, w)
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        // Accept a pack or instrument name for Instrument ("salamander").
        if id.get() as usize == P_INSTRUMENT {
            if let Ok(s) = t.to_str() {
                let q = s.trim().to_lowercase();
                if q.parse::<f64>().is_err() && !q.is_empty() {
                    let lib = self.shared.library.lock();
                    if let Some(i) = lib.iter().position(|e| format!("{} / {}", e.pack, e.name).to_lowercase().contains(&q)) {
                        return Some(i as f64);
                    }
                }
            }
        }
        superduper_dsp_sdk::clap_helpers::ParamDef::text_to_value_by_display(PARAMS, id, t, |v, s| self.display_text(id, v, s))
    }
    fn flush(&mut self, ev: &InputEvents, _: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, ev: &InputEvents, _: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PianoState {
    version: u32,
    params: Vec<f32>,
    bypass: bool,
    /// The loaded `.sfz` — survives packs being added (which shifts indices).
    #[serde(default)]
    instrument_path: Option<PathBuf>,
}

const STATE_VERSION: u32 = 1;

impl PluginStateImpl for PluginMainThread<'_> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        let state = PianoState {
            version: STATE_VERSION,
            params: self.shared.params.iter().map(|a| a.load(Ordering::Relaxed)).collect(),
            bypass: self.shared.bypass.load(Ordering::Relaxed),
            instrument_path: self.shared.state_path.lock().clone()
                .or_else(|| self.shared.info.lock().path.clone()),
        };
        serde_json::to_writer(output, &state).map_err(|_| PluginError::Message("state JSON error"))
    }

    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        let state: PianoState = serde_json::from_reader(input).map_err(|_| PluginError::Message("state JSON error"))?;
        if state.version != STATE_VERSION { return Err(PluginError::Message("state version mismatch")); }
        for (i, v) in state.params.iter().enumerate() {
            if let Some(slot) = self.shared.params.get(i) { slot.store(*v, Ordering::Relaxed); }
        }
        self.shared.bypass.store(state.bypass, Ordering::Relaxed);
        self.shared.param_seen.store(self.shared.params[P_INSTRUMENT].load(Ordering::Relaxed).round() as i32, Ordering::Relaxed);
        if let Some(p) = state.instrument_path {
            request_path(&self.shared.inner, p);
        } else {
            // No path saved: let the index decide.
            self.shared.current_index.store(-2, Ordering::Relaxed);
            let idx = self.shared.params[P_INSTRUMENT].load(Ordering::Relaxed).round() as usize;
            if let Some(p) = self.shared.library.lock().get(idx).map(|e| e.path.clone()) {
                request_path(&self.shared.inner, p);
            }
        }
        self.track_watch.rescan_param_values();
        Ok(())
    }
}

use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window as ClapGuiWindow,
};

impl PluginGuiImpl for PluginMainThread<'_> {
    fn is_api_supported(&mut self, c: GuiConfiguration) -> bool {
        if c.is_floating { return false; }
        c.api_type == GuiApiType::COCOA || c.api_type == GuiApiType::WIN32 || c.api_type == GuiApiType::X11
    }
    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        let api_type = if cfg!(target_os = "macos") { GuiApiType::COCOA }
            else if cfg!(target_os = "windows") { GuiApiType::WIN32 }
            else { GuiApiType::X11 };
        Some(GuiConfiguration { api_type, is_floating: false })
    }
    fn create(&mut self, _: GuiConfiguration) -> Result<(), PluginError> { Ok(()) }
    fn destroy(&mut self) { self.gui_handle = None; }
    fn set_scale(&mut self, _: f64) -> Result<(), PluginError> { Ok(()) }
    fn get_size(&mut self) -> Option<GuiSize> {
        Some(GuiSize { width: self.gui_resize.0.load(Ordering::Relaxed), height: self.gui_resize.1.load(Ordering::Relaxed) })
    }
    fn can_resize(&mut self) -> bool { true }
    fn get_resize_hints(&mut self) -> Option<GuiResizeHints> {
        Some(GuiResizeHints { can_resize_horizontally: true, can_resize_vertically: true, strategy: AspectRatioStrategy::Disregard })
    }
    fn adjust_size(&mut self, s: GuiSize) -> Option<GuiSize> {
        Some(GuiSize {
            width: s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH),
            height: s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT),
        })
    }
    fn set_size(&mut self, s: GuiSize) -> Result<(), PluginError> {
        self.gui_resize.0.store(s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH), Ordering::Relaxed);
        self.gui_resize.1.store(s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT), Ordering::Relaxed);
        Ok(())
    }
    fn set_parent(&mut self, window: ClapGuiWindow) -> Result<(), PluginError> {
        self.gui_handle = Some(gui::open_window(&window, self.shared.shared_handle(), self.gui_resize.clone()));
        Ok(())
    }
    fn show(&mut self) -> Result<(), PluginError> { Ok(()) }
    fn hide(&mut self) -> Result<(), PluginError> { Ok(()) }
    fn set_transient(&mut self, _: ClapGuiWindow) -> Result<(), PluginError> { Ok(()) }
}

pub struct SuperDuperPiano;

impl Plugin for SuperDuperPiano {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::track_info::PluginTrackInfo>();
    }
}

/// Spawn the loader thread. Holds a `Weak`, so it ends with the instance.
pub fn spawn_loader(shared: &SharedParams) {
    let weak = Arc::downgrade(shared);
    let _ = std::thread::Builder::new()
        .name("sdsp-piano-loader".into())
        .spawn(move || loop {
            match weak.upgrade() {
                Some(inner) => { loader_step(&inner); drop(inner); }
                None => break,
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        });
}

impl DefaultPluginFactory for SuperDuperPiano {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("co.superduperai.piano", plugin_display_name!("SuperDuper Piano"))
            .with_vendor("SuperDuperAI")
            .with_version(version_string!("0.1"))
            .with_description("SFZ multisample player — pianos and any SFZ pack in ~/Music/SFZ")
            .with_features([INSTRUMENT, STEREO, SAMPLER])
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        let shared = PluginShared::new();
        // Scanning is a directory walk — cheap enough for plugin scan time.
        // Decoding waits for activate().
        let n = refresh_library(&shared.inner);
        slog!("Piano new_shared: {} instruments under {}", n, shared.pack_root.display());
        spawn_loader(&shared.inner);
        superduper_synth_core::gui::spawn_bus_publisher_mcp("SuperDuper Piano", &shared.inner, |s| &s.scope, Some(superduper_synth_core::gui::ParamsAccess { defs: PARAMS, values: |s| &s.params, dirty: Some(|s| &s.dirty_params) }), None);
        Ok(shared)
    }
    fn new_main_thread<'a>(host: HostMainThreadHandle<'a>, shared: &'a PluginShared) -> Result<PluginMainThread<'a>, PluginError> {
        let track_watch = superduper_synth_core::gui::TrackInfoWatch::new(host, &shared.scope);
        Ok(PluginMainThread {
            track_watch, shared, gui_handle: None, gui_resize: gui::new_resize_bridge() })
    }
}

clack_export_entry!(SinglePluginEntry<SuperDuperPiano>);

impl clack_extensions::track_info::PluginTrackInfoImpl for PluginMainThread<'_> {
    fn changed(&self) {
        self.track_watch.refresh();
    }
}
