//! SuperDuper Vocoder — classic 16-band channel vocoder as a standalone
//! CLAP plugin. Tuned for the Daft Punk / Kraftwerk robot voice.
//!
//! The DSP lives in `dsp.rs` (pure, CLAP-free, driven directly by tests);
//! this file is the CLAP plumbing: params, audio ports (main + sidechain
//! carrier), state, and the egui GUI window.

#![allow(clippy::missing_safety_doc)]

pub mod dsp;
pub mod gui;
pub mod presets;
pub mod viz;

pub use dsp::{VocParams, Vocoder};

use atomic_float::AtomicF32;
use clack_common::events::spaces::CoreEventSpace;
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
use clack_extensions::state::PluginState;

use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use superduper_dsp_sdk::clap_helpers::{split_io, ParamDef};
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};

fn init_logging() {
    superduper_dsp_sdk::log::init("vocoder");
}
use superduper_dsp_sdk::slog;

// ===========================================================================
// Parameter table — FROZEN once shipped (REAPER caches the layout per slot).
// ===========================================================================

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0,  name: b"Attack",   min: 0.5,   max: 50.0,  default: 3.0,  unit: "ms" },
    ParamDef { id: 1,  name: b"Release",  min: 5.0,   max: 300.0, default: 25.0, unit: "ms" },
    // 0 = Internal oscillators, 1 = Sidechain input (port 1).
    ParamDef { id: 2,  name: b"Source",   min: 0.0,   max: 1.0,   default: 0.0,  unit: ""   },
    // 0 = Saw, 1 = Square, 2 = Pulse, 3 = Saw+Sub.
    ParamDef { id: 3,  name: b"Wave",     min: 0.0,   max: 3.0,   default: 0.0,  unit: ""   },
    ParamDef { id: 4,  name: b"Pitch",    min: -24.0, max: 24.0,  default: 0.0,  unit: "st" },
    ParamDef { id: 5,  name: b"Detune",   min: 0.0,   max: 25.0,  default: 0.0,  unit: "ct" },
    ParamDef { id: 6,  name: b"Formant",  min: -12.0, max: 12.0,  default: 0.0,  unit: "st" },
    ParamDef { id: 7,  name: b"Unvoiced", min: 0.0,   max: 1.0,   default: 0.15, unit: ""   },
    ParamDef { id: 8,  name: b"Drive",    min: 0.0,   max: 1.0,   default: 0.0,  unit: ""   },
    ParamDef { id: 9,  name: b"Mix",      min: 0.0,   max: 1.0,   default: 1.0,  unit: ""   },
    ParamDef { id: 10, name: b"Output",   min: -24.0, max: 24.0,  default: 0.0,  unit: "dB" },
    // 0 = 11 bands (tinny), 1 = 16 (default), 2 = 20 (intelligible).
    ParamDef { id: 11, name: b"Bands",    min: 0.0,   max: 2.0,   default: 1.0,  unit: ""   },
    // Internal carrier pitch: 0 = Auto (MIDI keys if held, else Voice/YIN),
    // 1 = MIDI (keys only), 2 = Voice (YIN track only).
    ParamDef { id: 12, name: b"Pitch Src", min: 0.0,  max: 2.0,   default: 0.0,  unit: ""   },
    // Engine: 0 = Classic (multi-band channel vocoder), 1 = Spectral (FFT
    // cross-synthesis — finer, whole-spectrum formant transfer).
    ParamDef { id: 13, name: b"Mode",     min: 0.0,   max: 1.0,   default: 0.0,  unit: ""   },
    // Spectral-mode formant-envelope resolution: 0 = Low (broad), 1 = Mid,
    // 2 = High, 3 = Ultra (fine). Only meaningful in Spectral; Classic uses Bands.
    ParamDef { id: 14, name: b"Detail",   min: 0.0,   max: 3.0,   default: 1.0,  unit: ""   },
];

pub const P_ATTACK: usize = 0;
pub const P_RELEASE: usize = 1;
pub const P_SOURCE: usize = 2;
pub const P_WAVE: usize = 3;
pub const P_PITCH: usize = 4;
pub const P_DETUNE: usize = 5;
pub const P_FORMANT: usize = 6;
pub const P_UNVOICED: usize = 7;
pub const P_DRIVE: usize = 8;
pub const P_MIX: usize = 9;
pub const P_OUTPUT: usize = 10;
pub const P_BANDS: usize = 11;
pub const P_PITCH_SOURCE: usize = 12;
pub const P_MODE: usize = 13;
pub const P_DETAIL: usize = 14;

// ===========================================================================
// Shared params (Arc so the egui thread can clone a handle).
// ===========================================================================

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: std::sync::atomic::AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub dirty_params: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_begin: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_end: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub active_preset: std::sync::atomic::AtomicU32,
    /// STFT latency reported to the host for PDC (same for both modes so
    /// switching Mode never re-triggers host plugin-delay compensation).
    pub latency_samples: std::sync::atomic::AtomicU32,
    /// Lock-free vocoder-activity snapshot for the GUI (bars / formant curve).
    pub viz: viz::VocViz,
}

pub struct PluginShared {
    pub inner: SharedParams,
}

impl PluginShared {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: std::sync::atomic::AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                active_preset: std::sync::atomic::AtomicU32::new(0),
                latency_samples: std::sync::atomic::AtomicU32::new(0),
                viz: viz::VocViz::new(),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
            }),
        }
    }
    pub fn shared_handle(&self) -> SharedParams {
        std::sync::Arc::clone(&self.inner)
    }
}

impl Default for PluginShared {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Deref for PluginShared {
    type Target = SharedParamsInner;
    fn deref(&self) -> &SharedParamsInner {
        &self.inner
    }
}

impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

// ===========================================================================
// Main thread / audio processor.
// ===========================================================================

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    voc: Box<Vocoder>,
    // Pre-allocated sidechain (carrier) scratch — filled from input port 1
    // each block. Never touch the allocator on the audio thread.
    sc_l: Box<[f32]>,
    sc_r: Box<[f32]>,
    /// Held MIDI notes, one per carrier voice slot (`-1` = empty). Updated
    /// from NoteOn/NoteOff in the event walk; passed to the DSP each block.
    held: [i16; dsp::MAX_VOICES],
    /// Round-robin index for voice stealing when all slots are full.
    steal: usize,
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
}

impl PluginAudioProcessor<'_> {
    fn note_on(&mut self, key: u8) {
        let k = key as i16;
        // Ignore duplicate NoteOn for a key already sounding.
        if self.held.iter().any(|&n| n == k) {
            return;
        }
        if let Some(slot) = self.held.iter().position(|&n| n < 0) {
            self.held[slot] = k;
        } else {
            // All slots busy — steal round-robin.
            let slot = self.steal % dsp::MAX_VOICES;
            self.held[slot] = k;
            self.steal = self.steal.wrapping_add(1);
        }
    }

    fn note_off(&mut self, key: u8) {
        let k = key as i16;
        for n in self.held.iter_mut() {
            if *n == k {
                *n = -1;
            }
        }
    }

    fn all_notes_off(&mut self) {
        self.held = [-1; dsp::MAX_VOICES];
    }

    /// Walk the event stream for note on/off (CLAP + raw MIDI) and update the
    /// held-note slots. Note events never touch the param dirty flags.
    fn handle_note_events(&mut self, events: &InputEvents) {
        for event in events {
            let Some(core) = event.as_core_event() else { continue };
            match core {
                CoreEventSpace::NoteOn(n) => {
                    if let Match::Specific(k) = n.key() {
                        self.note_on(k as u8);
                    }
                }
                CoreEventSpace::NoteOff(n) => match n.key() {
                    Match::Specific(k) => self.note_off(k as u8),
                    Match::All => self.all_notes_off(),
                },
                CoreEventSpace::NoteChoke(n) => match n.key() {
                    Match::Specific(k) => self.note_off(k as u8),
                    Match::All => self.all_notes_off(),
                },
                CoreEventSpace::Midi(m) => {
                    let data = m.data();
                    let status = data[0] & 0xf0;
                    let key = data[1];
                    let vel = data[2];
                    match status {
                        0x90 if vel > 0 => self.note_on(key),
                        0x80 | 0x90 => self.note_off(key), // 0x90 vel0 = note off
                        0xb0 if key == 123 || key == 120 => self.all_notes_off(),
                        _ => {}
                    }
                }
                _ => {}
            }
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
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = audio_config.sample_rate as f32;
        slog!("activate: sr={}", sr);
        let max_frames = audio_config.max_frames_count as usize;
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        let mut voc = Box::new(Vocoder::new(sr));
        let out_lin = 10f32.powf(load(P_OUTPUT) / 20.0);
        voc.prime(load(P_MIX), load(P_UNVOICED), load(P_DRIVE), out_lin);
        shared
            .latency_samples
            .store(voc.latency_samples(), Ordering::Relaxed);
        Ok(Self {
            shared,
            voc,
            sc_l: vec![0.0; max_frames].into_boxed_slice(),
            sc_r: vec![0.0; max_frames].into_boxed_slice(),
            held: [-1; dsp::MAX_VOICES],
            steal: 0,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // Flush denormals — the band filters + envelope followers otherwise
        // spin up ~10⁻³⁸ floats that murder CPU on release tails.
        let _denormals = superduper_dsp_sdk::denormals::Guard::new();
        apply_param_events(self.shared, events.input);
        self.handle_note_events(events.input);
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(
            &self.shared.params,
            &self.shared.dirty_params,
            events.output,
        );
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(
            &self.shared.gesture_begin,
            &self.shared.gesture_end,
            events.output,
        );

        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let params = VocParams {
            attack_ms: load(P_ATTACK),
            release_ms: load(P_RELEASE),
            source: load(P_SOURCE).round() as u32,
            wave: load(P_WAVE).round() as u32,
            band_count: dsp::band_count_from_param(load(P_BANDS).round() as u32),
            pitch_source: load(P_PITCH_SOURCE).round() as u32,
            notes: self.held,
            pitch_offset_semi: load(P_PITCH),
            detune_cents: load(P_DETUNE),
            formant_semi: load(P_FORMANT),
            unvoiced: load(P_UNVOICED),
            drive: load(P_DRIVE),
            mix: load(P_MIX),
            output_lin: 10f32.powf(load(P_OUTPUT) / 20.0),
            mode: load(P_MODE).round() as u32,
            detail: load(P_DETAIL).round() as u32,
            bypassed: self.shared.bypass.load(Ordering::Relaxed),
        };

        // ---- Snapshot the sidechain carrier (input port 1) -----------------
        let frames = audio.frames_count() as usize;
        let n_frames = frames.min(self.sc_l.len());
        self.sc_l[..n_frames].fill(0.0);
        self.sc_r[..n_frames].fill(0.0);
        if let Some(sc_port) = audio.input_port(1) {
            if let Some(chans) = sc_port.channels()?.into_f32() {
                if let Some(l) = chans.channel(0) {
                    let k = n_frames.min(l.len());
                    self.sc_l[..k].copy_from_slice(&l[..k]);
                }
                if let Some(r) = chans.channel(1) {
                    let k = n_frames.min(r.len());
                    self.sc_r[..k].copy_from_slice(&r[..k]);
                } else {
                    // Mono sidechain → mirror into R.
                    self.sc_r[..n_frames].copy_from_slice(&self.sc_l[..n_frames]);
                }
            }
        }

        // ---- Process the main port (index 0) -------------------------------
        if let Some(mut main_pair) = audio.port_pair(0) {
            let Some(channel_pairs) = main_pair.channels()?.into_f32() else {
                return Ok(ProcessStatus::Continue);
            };
            let mut iter = channel_pairs.into_iter();
            let Some(ch_l) = iter.next() else {
                return Ok(ProcessStatus::Continue);
            };
            let ch_r = iter.next();

            let Some((read_l, write_l)) = split_io(ch_l) else {
                return Ok(ProcessStatus::Continue);
            };
            let (read_r, write_r): (&[f32], Option<&mut [f32]>) = match ch_r {
                Some(c) => match split_io(c) {
                    Some((r, w)) => (r, Some(w)),
                    None => (read_l, None),
                },
                None => (read_l, None),
            };

            let sc_l = &self.sc_l[..n_frames];
            let sc_r = &self.sc_r[..n_frames];
            match write_r {
                Some(w) => {
                    self.voc
                        .process_stereo(read_l, read_r, write_l, w, sc_l, sc_r, &params);
                }
                None => {
                    let empty: &mut [f32] = &mut [];
                    self.voc
                        .process_stereo(read_l, read_r, write_l, empty, sc_l, sc_r, &params);
                }
            }

            // Feed the spectrum strip from the (mono) left output.
            for &s in write_l.iter() {
                self.shared.scope.push(s);
            }
        }

        // ---- Publish the vocoder-activity snapshot (lock-free) -------------
        if !params.bypassed {
            if params.mode == dsp::MODE_SPECTRAL {
                let mut curve = [0.0f32; viz::VIZ_CURVE];
                self.voc.write_env_curve(&mut curve);
                self.shared.viz.write_curve(&curve);
            } else {
                let active = params.band_count.clamp(2, dsp::MAX_BANDS);
                self.shared.viz.write_bars(self.voc.viz_bars(), active);
            }
        }

        Ok(ProcessStatus::Continue)
    }
}

// ===========================================================================
// CLAP audio ports — main stereo I/O + a stereo sidechain carrier input.
// ===========================================================================

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input {
            2
        } else {
            1
        }
    }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        match (index, is_input) {
            (0, _) => writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: if is_input { b"Input" } else { b"Output" },
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: Some(ClapId::new(0)),
            }),
            (1, true) => writer.set(&AudioPortInfo {
                id: ClapId::new(1),
                name: b"Carrier",
                channel_count: 2,
                flags: AudioPortFlags::empty(),
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            }),
            _ => {}
        }
    }
}

impl PluginNotePortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input {
            1
        } else {
            0
        }
    }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if !is_input || index != 0 {
            return;
        }
        writer.set(&NotePortInfo {
            id: ClapId::new(0),
            name: b"Carrier Notes",
            // Advertise both dialects — a host that doesn't speak native CLAP
            // note events falls back to MIDI 1.0 (gotcha #14).
            supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
            preferred_dialect: Some(NoteDialect::Clap),
        });
    }
}

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 {
        PARAMS.len() as u32
    }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info(PARAMS, idx, info);
    }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        let i = id.get() as usize;
        self.shared
            .params
            .get(i)
            .map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(
        &mut self,
        id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        use core::fmt::Write;
        let pid = id.get() as usize;
        if pid == P_SOURCE {
            return write!(writer, "{}", if value < 0.5 { "Internal" } else { "Sidechain" });
        }
        if pid == P_WAVE {
            let names = ["Saw", "Square", "Pulse", "Saw+Sub"];
            let i = (value.round() as usize).min(names.len() - 1);
            return write!(writer, "{}", names[i]);
        }
        if pid == P_BANDS {
            let names = ["11", "16", "20"];
            let i = (value.round() as usize).min(names.len() - 1);
            return write!(writer, "{}", names[i]);
        }
        if pid == P_PITCH_SOURCE {
            let names = ["Auto", "MIDI", "Voice"];
            let i = (value.round() as usize).min(names.len() - 1);
            return write!(writer, "{}", names[i]);
        }
        if pid == P_MODE {
            return write!(writer, "{}", if value < 0.5 { "Classic" } else { "Spectral" });
        }
        if pid == P_DETAIL {
            let names = ["Low", "Mid", "High", "Ultra"];
            let i = (value.round() as usize).min(names.len() - 1);
            return write!(writer, "{}", names[i]);
        }
        ParamDef::write_display(PARAMS, id, value, writer)
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        ParamDef::parse_text(PARAMS, id, t)
    }
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        apply_param_events(self.shared, ev);
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        apply_param_events(self.shared, ev);
    }
}

// ===========================================================================
// CLAP state — params + bypass through the shared SDK helper.
// ===========================================================================

superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);

impl clack_extensions::latency::PluginLatencyImpl for PluginMainThread<'_> {
    fn get(&mut self) -> u32 {
        self.shared.latency_samples.load(Ordering::Relaxed)
    }
}

// ===========================================================================
// CLAP GUI extension.
// ===========================================================================

use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window as ClapGuiWindow,
};
use std::sync::atomic::Ordering as AtomicOrdering;

impl PluginGuiImpl for PluginMainThread<'_> {
    fn is_api_supported(&mut self, c: GuiConfiguration) -> bool {
        if c.is_floating {
            return false;
        }
        c.api_type == GuiApiType::COCOA
            || c.api_type == GuiApiType::WIN32
            || c.api_type == GuiApiType::X11
    }
    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        let api_type = if cfg!(target_os = "macos") {
            GuiApiType::COCOA
        } else if cfg!(target_os = "windows") {
            GuiApiType::WIN32
        } else {
            GuiApiType::X11
        };
        Some(GuiConfiguration {
            api_type,
            is_floating: false,
        })
    }
    fn create(&mut self, _: GuiConfiguration) -> Result<(), PluginError> {
        slog!("gui::create");
        Ok(())
    }
    fn destroy(&mut self) {
        slog!("gui::destroy");
        self.gui_handle = None;
    }
    fn set_scale(&mut self, _: f64) -> Result<(), PluginError> {
        Ok(())
    }
    fn get_size(&mut self) -> Option<GuiSize> {
        Some(GuiSize {
            width: self.gui_resize.0.load(AtomicOrdering::Relaxed),
            height: self.gui_resize.1.load(AtomicOrdering::Relaxed),
        })
    }
    fn can_resize(&mut self) -> bool {
        true
    }
    fn get_resize_hints(&mut self) -> Option<GuiResizeHints> {
        Some(GuiResizeHints {
            can_resize_horizontally: true,
            can_resize_vertically: true,
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
        self.gui_resize.0.store(w, AtomicOrdering::Relaxed);
        self.gui_resize.1.store(h, AtomicOrdering::Relaxed);
        Ok(())
    }
    fn set_parent(&mut self, window: ClapGuiWindow) -> Result<(), PluginError> {
        slog!("gui::set_parent");
        let handle =
            gui::open_window(&window, self.shared.shared_handle(), self.gui_resize.clone());
        self.gui_handle = Some(handle);
        Ok(())
    }
    fn set_transient(&mut self, _: ClapGuiWindow) -> Result<(), PluginError> {
        Ok(())
    }
    fn show(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
    fn hide(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
}

// ===========================================================================
// Factory.
// ===========================================================================

pub struct SuperDuperVocoder;

impl Plugin for SuperDuperVocoder {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::latency::PluginLatency>()
            .register::<clack_extensions::gui::PluginGui>();
    }
}

impl DefaultPluginFactory for SuperDuperVocoder {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.vocoder",
            plugin_display_name!("SuperDuper Vocoder"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Classic 16-band channel vocoder — robot voice / talkbox")
        .with_features([AUDIO_EFFECT, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Vocoder — build {} ({})", build_num!(), build_date!());
        Ok(PluginShared::new())
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        shared: &'a PluginShared,
    ) -> Result<PluginMainThread<'a>, PluginError> {
        Ok(PluginMainThread {
            shared,
            gui_handle: None,
            gui_resize: gui::new_resize_bridge(),
        })
    }
}

clack_export_entry!(SinglePluginEntry<SuperDuperVocoder>);
