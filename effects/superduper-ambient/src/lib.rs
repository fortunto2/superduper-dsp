//! SuperDuper Ambient — autonomous pad-drone generator.
//!
//! No MIDI input, no audio input. The plugin generates a continuous
//! chord-pad according to its parameters: root frequency, three voicing
//! intervals (chord-style), cutoff, resonance, drive, modulation depth,
//! master gain. Output is stereo with detune width — the left and right
//! channels each run their own set of PadVoices with slightly different
//! LFOs, producing natural stereo phasing.
//!
//! Use cases:
//!   - cinematic underscore beds
//!   - long evolving textures
//!   - meditation / ambient soundscapes
//!   - send-target for reverb / supermass so you have a sound to send
//!
//! Voice count: 3 stacked partials (root + interval1 + interval2 + interval3)
//! per channel = 6 total. Cheap CPU.

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod presets;

use atomic_float::AtomicF32;
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams,
    PluginParams,
};
use clack_common::stream::{InputStream, OutputStream};
use clack_extensions::state::{PluginState, PluginStateImpl};

use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{PadParams, PadVoice, SmoothedParam};

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn init_logging() { superduper_dsp_sdk::log::init("ambient"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Params — 7 knobs + 4 voice intervals (semitones from root)
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    // Root frequency (Hz). 55 Hz = A1, classic deep bass pad root.
    ParamDef { id: 0, name: b"Root",        min: 20.0,  max: 500.0,  default: 55.0,  unit: "Hz" },
    // Chord interval 1 — semitones above root. Default 7 = perfect fifth.
    ParamDef { id: 1, name: b"Voice 2",     min: 0.0,   max: 24.0,   default: 7.0,   unit: "st" },
    // Interval 2 — default 12 = octave.
    ParamDef { id: 2, name: b"Voice 3",     min: 0.0,   max: 24.0,   default: 12.0,  unit: "st" },
    // Interval 3 — default 19 = octave + fifth.
    ParamDef { id: 3, name: b"Voice 4",     min: 0.0,   max: 36.0,   default: 19.0,  unit: "st" },
    // Filter
    ParamDef { id: 4, name: b"Cutoff",      min: 80.0,  max: 12000.0, default: 1600.0, unit: "Hz" },
    ParamDef { id: 5, name: b"Resonance",   min: 0.0,   max: 0.9,    default: 0.2,   unit: ""   },
    // Modulation depth in cents — drifts each partial's pitch.
    ParamDef { id: 6, name: b"Modulation",  min: 0.0,   max: 50.0,   default: 8.0,   unit: "cents" },
    // Saturation drive into the post-filter stage.
    ParamDef { id: 7, name: b"Drive",       min: 0.0,   max: 1.0,    default: 0.3,   unit: ""   },
    // Stereo detune — L vs R fundamental drift in cents.
    ParamDef { id: 8, name: b"Width",       min: 0.0,   max: 30.0,   default: 7.0,   unit: "cents" },
    // Master output.
    ParamDef { id: 9, name: b"Output",      min: -36.0, max: 6.0,    default: -12.0, unit: "dB" },
];

pub const P_ROOT: usize = 0;
pub const P_VOICE2: usize = 1;
pub const P_VOICE3: usize = 2;
pub const P_VOICE4: usize = 3;
pub const P_CUTOFF: usize = 4;
pub const P_RESONANCE: usize = 5;
pub const P_MODULATION: usize = 6;
pub const P_DRIVE: usize = 7;
pub const P_WIDTH: usize = 8;
pub const P_OUTPUT: usize = 9;

// ---------------------------------------------------------------------------
// Shared params
// ---------------------------------------------------------------------------

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: std::sync::atomic::AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub dirty_params: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_begin: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_end: [std::sync::atomic::AtomicBool; PARAMS.len()],
}

pub struct PluginShared { pub inner: SharedParams }

impl PluginShared {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: std::sync::atomic::AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
            }),
        }
    }
    pub fn shared_handle(&self) -> SharedParams { std::sync::Arc::clone(&self.inner) }
}

impl Default for PluginShared { fn default() -> Self { Self::new() } }
impl std::ops::Deref for PluginShared {
    type Target = SharedParamsInner;
    fn deref(&self) -> &SharedParamsInner { &self.inner }
}
impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

// ---------------------------------------------------------------------------
// Audio processor — owns one PadVoice per channel × 4 chord positions.
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    voices_l: [PadVoice; 4],
    voices_r: [PadVoice; 4],
    smooth_root: SmoothedParam,
    smooth_cutoff: SmoothedParam,
    smooth_resonance: SmoothedParam,
    smooth_modulation: SmoothedParam,
    smooth_drive: SmoothedParam,
    smooth_width: SmoothedParam,
    smooth_output: SmoothedParam,
    sample_rate: f32,
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
}

/// Convert semitones to a frequency multiplier (2^(n/12)).
#[inline]
fn semitone_ratio(semitones: f32) -> f32 {
    2f32.powf(semitones / 12.0)
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
        slog!("activate sr={}", sr);
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        Ok(Self {
            shared,
            voices_l: Default::default(),
            voices_r: Default::default(),
            smooth_root: SmoothedParam::new(load(P_ROOT)),
            smooth_cutoff: SmoothedParam::new(load(P_CUTOFF)),
            smooth_resonance: SmoothedParam::new(load(P_RESONANCE)),
            smooth_modulation: SmoothedParam::new(load(P_MODULATION)),
            smooth_drive: SmoothedParam::new(load(P_DRIVE)),
            smooth_width: SmoothedParam::new(load(P_WIDTH)),
            smooth_output: SmoothedParam::new(load(P_OUTPUT)),
            sample_rate: sr,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        apply_param_events(self.shared, events.input);
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(
            &self.shared.params, &self.shared.dirty_params, events.output);
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(
            &self.shared.gesture_begin,
            &self.shared.gesture_end,
            events.output,
        );

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        let sr = self.sample_rate;

        let root_target = self.shared.params[P_ROOT].load(Ordering::Relaxed);
        let v2 = self.shared.params[P_VOICE2].load(Ordering::Relaxed);
        let v3 = self.shared.params[P_VOICE3].load(Ordering::Relaxed);
        let v4 = self.shared.params[P_VOICE4].load(Ordering::Relaxed);
        let cutoff_target = self.shared.params[P_CUTOFF].load(Ordering::Relaxed);
        let resonance_target = self.shared.params[P_RESONANCE].load(Ordering::Relaxed);
        let modulation_target = self.shared.params[P_MODULATION].load(Ordering::Relaxed);
        let drive_target = self.shared.params[P_DRIVE].load(Ordering::Relaxed);
        let width_target = self.shared.params[P_WIDTH].load(Ordering::Relaxed);
        let output_target = self.shared.params[P_OUTPUT].load(Ordering::Relaxed);

        // Pre-compute chord ratios (cheap, once per block).
        let ratios = [1.0, semitone_ratio(v2), semitone_ratio(v3), semitone_ratio(v4)];

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            // We only need the output side — Ambient generates audio.
            for (ch_idx, channel_pair) in channel_pairs.into_iter().enumerate() {
                use superduper_dsp_sdk::clap_helpers::output_slice;
                let write = match output_slice(channel_pair) {
                    Some(w) => w,
                    None => continue,
                };
                if bypassed {
                    write.fill(0.0);
                    continue;
                }

                let voices = if ch_idx == 0 { &mut self.voices_l } else { &mut self.voices_r };

                for o in write.iter_mut() {
                    let root = self.smooth_root.step(root_target, sr);
                    let cutoff = self.smooth_cutoff.step(cutoff_target, sr);
                    let resonance = self.smooth_resonance.step(resonance_target, sr);
                    let modulation = self.smooth_modulation.step(modulation_target, sr);
                    let drive = self.smooth_drive.step(drive_target, sr);
                    let width = self.smooth_width.step(width_target, sr);
                    let output_db = self.smooth_output.step(output_target, sr);

                    // Stereo width: detune L vs R by ±width/2 cents on the root.
                    let stereo_cents = if ch_idx == 0 { -width * 0.5 } else { width * 0.5 };
                    let stereo_ratio = 2f32.powf(stereo_cents / 1200.0);

                    // Sum the four chord voices for this channel.
                    let mut mix = 0.0_f32;
                    for (vi, voice) in voices.iter_mut().enumerate() {
                        let p = PadParams {
                            sr,
                            root_hz: root * ratios[vi] * stereo_ratio,
                            cutoff_hz: cutoff,
                            resonance,
                            modulation_cents: modulation,
                            drive,
                        };
                        mix += voice.process(p);
                    }
                    mix *= 0.25; // normalise — 4 voices

                    let out_lin = 10f32.powf(output_db / 20.0);
                    let final_sample = mix * out_lin;
                    *o = final_sample;
                    self.shared.scope.push(final_sample);
                }
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        // Generator — no audio input, single stereo output.
        if is_input { 0 } else { 1 }
    }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if is_input || index != 0 { return; }
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: b"Output",
            channel_count: 2,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            in_place_pair: None,
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
        ParamDef::write_display(PARAMS, id, v, w)
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

// ---------------------------------------------------------------------------
// CLAP state — params + bypass through the shared SDK helper. Without this
// REAPER drops everything when saving the project / FX chain preset.
// ---------------------------------------------------------------------------

superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);


use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window as ClapGuiWindow,
};
use std::sync::atomic::Ordering as AtomicOrdering;

impl PluginGuiImpl for PluginMainThread<'_> {
    fn is_api_supported(&mut self, c: GuiConfiguration) -> bool {
        if c.is_floating { return false; }
        c.api_type == GuiApiType::COCOA
            || c.api_type == GuiApiType::WIN32
            || c.api_type == GuiApiType::X11
    }
    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        let api_type = if cfg!(target_os = "macos") { GuiApiType::COCOA }
            else if cfg!(target_os = "windows") { GuiApiType::WIN32 }
            else { GuiApiType::X11 };
        Some(GuiConfiguration { api_type, is_floating: false })
    }
    fn create(&mut self, _: GuiConfiguration) -> Result<(), PluginError> { slog!("gui::create"); Ok(()) }
    fn destroy(&mut self) { slog!("gui::destroy"); self.gui_handle = None; }
    fn set_scale(&mut self, _: f64) -> Result<(), PluginError> { Ok(()) }
    fn get_size(&mut self) -> Option<GuiSize> {
        Some(GuiSize {
            width: self.gui_resize.0.load(AtomicOrdering::Relaxed),
            height: self.gui_resize.1.load(AtomicOrdering::Relaxed),
        })
    }
    fn can_resize(&mut self) -> bool { true }
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
        let handle = gui::open_window(&window, self.shared.shared_handle(), self.gui_resize.clone());
        self.gui_handle = Some(handle);
        Ok(())
    }
    fn set_transient(&mut self, _: ClapGuiWindow) -> Result<(), PluginError> { Ok(()) }
    fn show(&mut self) -> Result<(), PluginError> { Ok(()) }
    fn hide(&mut self) -> Result<(), PluginError> { Ok(()) }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct SuperDuperAmbient;

impl Plugin for SuperDuperAmbient {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;
    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>();
    }
}

impl DefaultPluginFactory for SuperDuperAmbient {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.ambient",
            plugin_display_name!("SuperDuper Ambient"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.2"))
        .with_description("Autonomous ambient pad — drones a chord with slow modulation")
        .with_features([INSTRUMENT, STEREO, SYNTHESIZER])
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Ambient — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperAmbient>);
