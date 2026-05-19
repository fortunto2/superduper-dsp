//! SuperDuper Sampler — polyphonic WAV player. Scans known sample
//! folders on activate (~/Music/SuperDuper Samples/ +
//! ~/Music/Favorite 808s/) and plays the active WAV polyphonically
//! with pitch, ADSR and optional loop.
//!
//! Use case: load any one-shot — 808 kicks, vocal phrases,
//! breakbeats, percussion — and play across the keyboard.
//! Out-of-the-box bass synth too: pitch a low note + long Decay
//! and you get a hard-hit 808-style sub.

#![allow(clippy::missing_safety_doc)]

pub mod bank;
pub mod gui;
pub mod voice;

use atomic_float::AtomicF32;
use clack_common::events::Match;
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
use clack_common::stream::{InputStream, OutputStream};
use clack_extensions::state::{PluginState, PluginStateImpl};

use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use parking_lot::Mutex;
use superduper_dsp_sdk::clap_helpers::{output_slice, ParamDef};
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::AdsrParams;

use bank::{empty_sample, load_sample, scan_folders, SampleData};
use voice::{SampleVoice, VoiceParams, NOTE_FREE};

const VOICE_COUNT: usize = 8;

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn log_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".superduper-dsp")
        .join("sampler.log")
}

static LOG_FILE: std::sync::OnceLock<Mutex<Option<std::fs::File>>> =
    std::sync::OnceLock::new();

fn init_logging() {
    LOG_FILE.get_or_init(|| {
        let path = log_path();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        Mutex::new(std::fs::OpenOptions::new()
            .create(true).append(true).open(&path).ok())
    });
}

fn slog_args(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    if let Some(slot) = LOG_FILE.get() {
        if let Some(file) = slot.lock().as_mut() {
            let _ = writeln!(file, "{}", args);
        }
    }
}
macro_rules! slog { ($($arg:tt)*) => { $crate::slog_args(format_args!($($arg)*)) } }

// ---------------------------------------------------------------------------
// Param table
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    // Sample index — clamped at runtime to the actual scan results.
    // We expose 256 as the max so the host can record sample changes
    // without us re-broadcasting the param table on every scan.
    ParamDef { id: 0,  name: b"Sample",  min: 0.0,   max: 255.0,  default: 0.0,  unit: ""   },
    // Pitch root — MIDI key at which the sample plays at its original speed.
    ParamDef { id: 1,  name: b"Root",    min: 0.0,   max: 127.0,  default: 60.0, unit: ""   },
    ParamDef { id: 2,  name: b"Tune",    min: -24.0, max: 24.0,   default: 0.0,  unit: "ST" },
    ParamDef { id: 3,  name: b"Fine",    min: -100.0, max: 100.0, default: 0.0,  unit: "ct" },
    // Loop
    ParamDef { id: 4,  name: b"Loop",       min: 0.0, max: 1.0, default: 0.0, unit: "" },
    ParamDef { id: 5,  name: b"Loop Start", min: 0.0, max: 1.0, default: 0.0, unit: "" },
    ParamDef { id: 6,  name: b"Loop End",   min: 0.0, max: 1.0, default: 1.0, unit: "" },
    // ADSR
    ParamDef { id: 7,  name: b"Attack",  min: 0.0,   max: 4.0,  default: 0.001, unit: "s" },
    ParamDef { id: 8,  name: b"Decay",   min: 0.01,  max: 8.0,  default: 0.5,   unit: "s" },
    ParamDef { id: 9,  name: b"Sustain", min: 0.0,   max: 1.0,  default: 1.0,   unit: ""  },
    ParamDef { id: 10, name: b"Release", min: 0.01,  max: 8.0,  default: 0.4,   unit: "s" },
    // Output
    ParamDef { id: 11, name: b"Output",  min: -36.0, max: 6.0,  default: -3.0,  unit: "dB" },
];

pub const P_SAMPLE: usize = 0;
pub const P_ROOT: usize = 1;
pub const P_TUNE: usize = 2;
pub const P_FINE: usize = 3;
pub const P_LOOP: usize = 4;
pub const P_LOOP_START: usize = 5;
pub const P_LOOP_END: usize = 6;
pub const P_ATTACK: usize = 7;
pub const P_DECAY: usize = 8;
pub const P_SUSTAIN: usize = 9;
pub const P_RELEASE: usize = 10;
pub const P_OUTPUT: usize = 11;

// ---------------------------------------------------------------------------
// Shared params + sample library
// ---------------------------------------------------------------------------

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub dirty_params: [AtomicBool; PARAMS.len()],
    pub gesture_begin: [AtomicBool; PARAMS.len()],
    pub gesture_end: [AtomicBool; PARAMS.len()],
    /// Active sample currently loaded — atomically swapped by the
    /// GUI thread when the user picks a different file. Audio thread
    /// clones the Arc when triggering a new voice; existing voices
    /// keep playing the previous sample until they finish.
    pub active_sample: Mutex<Arc<SampleData>>,
    /// Snapshot of the discovered sample files. The GUI picks an
    /// index from this list and triggers a load; the audio thread
    /// never touches it directly.
    pub library: Mutex<Vec<PathBuf>>,
    /// Currently-loaded library index. -1 = no sample yet.
    pub current_index: std::sync::atomic::AtomicI32,
    /// Plugin sample rate, captured at activate() so the GUI can show
    /// it in the status line.
    pub host_sr: AtomicF32,
}

pub struct PluginShared { pub inner: SharedParams }

impl PluginShared {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| AtomicBool::new(false)),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
                active_sample: Mutex::new(empty_sample()),
                library: Mutex::new(Vec::new()),
                current_index: std::sync::atomic::AtomicI32::new(-1),
                host_sr: AtomicF32::new(48000.0),
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

/// GUI helper: refresh the library scan, pick the i-th sample, decode
/// it and swap the active_sample Arc. Returns Ok(name) or Err(reason).
/// Called by the GUI when the user clicks a dropdown entry.
pub fn pick_sample(shared: &SharedParamsInner, idx: usize) -> Result<String, String> {
    let lib = shared.library.lock();
    let path = lib.get(idx)
        .cloned()
        .ok_or_else(|| format!("sample index {} out of range ({})", idx, lib.len()))?;
    drop(lib);
    let data = load_sample(&path).map_err(|e| e.to_string())?;
    let name = data.display_name.clone();
    *shared.active_sample.lock() = Arc::new(data);
    shared.current_index.store(idx as i32, Ordering::Relaxed);
    shared.params[P_SAMPLE].store(idx as f32, Ordering::Relaxed);
    shared.dirty_params[P_SAMPLE].store(true, Ordering::Relaxed);
    Ok(name)
}

/// GUI helper: rerun the folder scan and refresh the library. Returns
/// the new entry count.
pub fn refresh_library(shared: &SharedParamsInner) -> usize {
    let folders = bank::default_sample_folders();
    let entries = scan_folders(&folders);
    let count = entries.len();
    *shared.library.lock() = entries;
    count
}

// ---------------------------------------------------------------------------
// Main thread + audio processor
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    voices: [SampleVoice; VOICE_COUNT],
    next_age: u64,
    sample_rate: f32,
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
        init_logging();
        let sr = cfg.sample_rate as f32;
        shared.host_sr.store(sr, Ordering::Relaxed);
        slog!("sampler activate sr={}", sr);
        Ok(Self {
            shared,
            voices: std::array::from_fn(|_| SampleVoice::default()),
            next_age: 0,
            sample_rate: sr,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, events.input);
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(
            &self.shared.params, &self.shared.dirty_params, events.output);
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(
            &self.shared.gesture_begin, &self.shared.gesture_end, events.output);

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        let sr = self.sample_rate;

        // Walk events — NoteOn / NoteOff drive voice triggers.
        for batch in events.input.batch() {
            for ev in batch.events() {
                if let Some(core) = ev.as_core_event() {
                    match core {
                        CoreEventSpace::NoteOn(n) => {
                            let key = match n.key() {
                                Match::Specific(k) => k as u8,
                                _ => continue,
                            };
                            let velocity = n.velocity().clamp(0.0, 1.0) as f32;
                            let note_id = match n.note_id() {
                                Match::Specific(id) => id as i32,
                                _ => -1,
                            };
                            self.trigger(key, velocity, note_id);
                        }
                        CoreEventSpace::NoteOff(n) => {
                            let key = match n.key() {
                                Match::Specific(k) => k as u8,
                                _ => continue,
                            };
                            self.release(key);
                        }
                        CoreEventSpace::Midi(m) => {
                            let d = m.data();
                            let st = d[0] & 0xF0;
                            match st {
                                0x90 if d[2] > 0 => self.trigger(d[1], d[2] as f32 / 127.0, -1),
                                0x90 | 0x80 => self.release(d[1]),
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Audio render.
        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let params = VoiceParams {
            host_sr: sr,
            root_key: load(P_ROOT),
            tune_st: load(P_TUNE),
            fine_cents: load(P_FINE),
            loop_on: load(P_LOOP) >= 0.5,
            loop_start_frac: load(P_LOOP_START),
            loop_end_frac: load(P_LOOP_END),
            env: AdsrParams::adsr(sr, load(P_ATTACK), load(P_DECAY), load(P_SUSTAIN), load(P_RELEASE)),
            output_lin: 10f32.powf(load(P_OUTPUT) / 20.0),
        };

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            let mut writers: Vec<_> = channel_pairs.into_iter()
                .filter_map(output_slice).collect();
            if writers.len() < 2 {
                for w in writers.iter_mut() { w.fill(0.0); }
                continue;
            }
            let (a, b) = writers.split_at_mut(1);
            let out_l = a[0].as_mut();
            let out_r = b[0].as_mut();
            let frames = out_l.len().min(out_r.len());

            if bypassed {
                for i in 0..frames { out_l[i] = 0.0; out_r[i] = 0.0; }
                continue;
            }

            for i in 0..frames {
                let mut l = 0.0_f32;
                let mut r = 0.0_f32;
                for v in self.voices.iter_mut() {
                    let (vl, vr) = v.process(params);
                    l += vl;
                    r += vr;
                }
                out_l[i] = l;
                out_r[i] = r;
                self.shared.scope.push((l + r) * 0.5);
            }
        }

        Ok(ProcessStatus::Continue)
    }
}

impl<'a> PluginAudioProcessor<'a> {
    fn trigger(&mut self, key: u8, velocity: f32, note_id: i32) {
        self.next_age = self.next_age.wrapping_add(1);
        let stamp = self.next_age;
        // Find an idle voice or steal the oldest.
        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let params = VoiceParams {
            host_sr: self.sample_rate,
            root_key: load(P_ROOT),
            tune_st: load(P_TUNE),
            fine_cents: load(P_FINE),
            loop_on: load(P_LOOP) >= 0.5,
            loop_start_frac: load(P_LOOP_START),
            loop_end_frac: load(P_LOOP_END),
            env: AdsrParams::adsr(self.sample_rate, load(P_ATTACK), load(P_DECAY), load(P_SUSTAIN), load(P_RELEASE)),
            output_lin: 10f32.powf(load(P_OUTPUT) / 20.0),
        };
        let sample = Arc::clone(&self.shared.active_sample.lock());
        // Two-pass borrow: find the slot index without holding a
        // mutable reference, then re-grab the slot for the gate_on.
        let mut idle_idx: Option<usize> = None;
        let mut oldest_idx = 0usize;
        let mut oldest_age = u64::MAX;
        for (i, v) in self.voices.iter().enumerate() {
            if v.is_idle() && idle_idx.is_none() {
                idle_idx = Some(i);
            }
            if v.age_stamp < oldest_age {
                oldest_age = v.age_stamp;
                oldest_idx = i;
            }
        }
        let slot_idx = idle_idx.unwrap_or(oldest_idx);
        self.voices[slot_idx].gate_on(key, velocity, note_id, stamp, sample, params);
    }

    fn release(&mut self, key: u8) {
        for v in self.voices.iter_mut() {
            if v.key == key { v.gate_off(); v.key = NOTE_FREE; }
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

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 { PARAMS.len() as u32 }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info(PARAMS, idx, info);
    }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        let i = id.get() as usize;
        self.shared.params.get(i).map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(&mut self, id: ClapId, v: f64, w: &mut ParamDisplayWriter) -> core::fmt::Result {
        use core::fmt::Write;
        let pid = id.get() as usize;
        if pid == P_LOOP {
            return write!(w, "{}", if v >= 0.5 { "On" } else { "Off" });
        }
        if pid == P_SAMPLE {
            // Show the file stem instead of a number.
            let lib = self.shared.library.lock();
            if let Some(p) = lib.get(v.round() as usize) {
                if let Some(stem) = p.file_stem() {
                    return write!(w, "{}", stem.to_string_lossy());
                }
            }
        }
        ParamDef::write_display(PARAMS, id, v, w)
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        ParamDef::parse_text(PARAMS, id, t)
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

impl PluginStateImpl for PluginMainThread<'_> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        superduper_dsp_sdk::clap_helpers::save_simple_state(
            &self.shared.params,
            self.shared.bypass.load(Ordering::Relaxed),
            output,
        )
    }
    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        let bypass = superduper_dsp_sdk::clap_helpers::load_simple_state(
            &self.shared.params, input)?;
        self.shared.bypass.store(bypass, Ordering::Relaxed);
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
        Some(GuiSize {
            width: self.gui_resize.0.load(Ordering::Relaxed),
            height: self.gui_resize.1.load(Ordering::Relaxed),
        })
    }
    fn can_resize(&mut self) -> bool { true }
    fn get_resize_hints(&mut self) -> Option<GuiResizeHints> {
        Some(GuiResizeHints {
            can_resize_horizontally: true, can_resize_vertically: true,
            strategy: AspectRatioStrategy::Disregard,
        })
    }
    fn adjust_size(&mut self, s: GuiSize) -> Option<GuiSize> {
        Some(GuiSize {
            width: s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH),
            height: s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT),
        })
    }
    fn set_size(&mut self, s: GuiSize) -> Result<(), PluginError> {
        let w = s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH);
        let h = s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT);
        self.gui_resize.0.store(w, Ordering::Relaxed);
        self.gui_resize.1.store(h, Ordering::Relaxed);
        Ok(())
    }
    fn set_parent(&mut self, window: ClapGuiWindow) -> Result<(), PluginError> {
        let handle = gui::open_window(&window, self.shared.shared_handle(), self.gui_resize.clone());
        self.gui_handle = Some(handle);
        Ok(())
    }
    fn show(&mut self) -> Result<(), PluginError> { Ok(()) }
    fn hide(&mut self) -> Result<(), PluginError> { Ok(()) }
    fn set_transient(&mut self, _: ClapGuiWindow) -> Result<(), PluginError> { Ok(()) }
}

pub struct SuperDuperSampler;

impl Plugin for SuperDuperSampler {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>();
    }
}

impl DefaultPluginFactory for SuperDuperSampler {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("co.superduperai.sampler", plugin_display_name!("SuperDuper Sampler"))
            .with_vendor("SuperDuperAI")
            .with_version(version_string!("0.2"))
            .with_description("Polyphonic WAV sampler — scans known folders, plays any one-shot with pitch + ADSR + loop")
            .with_features([INSTRUMENT, STEREO, SAMPLER])
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        let shared = PluginShared::new();
        // Scan folders eagerly so the GUI dropdown is populated the
        // moment the user opens the window. Decoding is lazy — we
        // don't load any audio until they pick something.
        let count = refresh_library(&shared.inner);
        slog!("Sampler new_shared: found {} samples in default folders", count);
        Ok(shared)
    }
    fn new_main_thread<'a>(_host: HostMainThreadHandle<'a>, shared: &'a PluginShared)
        -> Result<PluginMainThread<'a>, PluginError>
    {
        Ok(PluginMainThread {
            shared, gui_handle: None, gui_resize: gui::new_resize_bridge(),
        })
    }
}

clack_export_entry!(SinglePluginEntry<SuperDuperSampler>);
