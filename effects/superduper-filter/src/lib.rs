//! SuperDuper Filter — multi-mode resonant filter for the master bus
//! (or anywhere else). The Daft Punk filter-sweep effect made into a
//! standalone plugin so it can sit on the master, drum bus, vocal,
//! aux send — any audio path, not just inside a synth.
//!
//! Modes (selectable via the Type param):
//!   - 0 = LP (classic moog-style sweep)
//!   - 1 = HP (the inverse — "telephone" / lo-fi narrowing)
//!   - 2 = BP (mid-band emphasis, vowel-like)
//!   - 3 = Notch (band-stop, surgical or wah-style)
//!
//! Signal chain (per sample):
//!   in → [optional Drive (Tanh/Tape/Tube) pre-filter]
//!      → SvfFilter (cutoff modulated by LFO + Env Follow)
//!      → [output gain] → Mix with dry
//!
//! LFO modulates Cutoff in semitones; Env Follow modulates Cutoff in
//! semitones too. Both stack additively. LFO has an optional tempo
//! sync mode driven by the host transport.

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

use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{
    tanh_drive, tape_clip, tube_clip, sync_division_hz, sync_division_label,
    EnvelopeDetector, SmoothedParam, SvfFilter, SvfMode,
};

fn init_logging() { superduper_dsp_sdk::log::init("filter"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Param table — 15 params
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    // 0 = LP, 1 = HP, 2 = BP, 3 = Notch
    ParamDef { id: 0,  name: b"Type",      min: 0.0,    max: 3.0,    default: 0.0,   unit: ""   },
    // Cutoff in "MIDI-like" units 0..127 → 20 Hz–20 kHz log mapping so
    // the slider feels musical (one octave per ~18 units).
    ParamDef { id: 1,  name: b"Cutoff",    min: 0.0,    max: 127.0,  default: 90.0,  unit: ""   },
    ParamDef { id: 2,  name: b"Reso",      min: 0.0,    max: 0.97,   default: 0.4,   unit: ""   },
    // Pre-filter drive 0..1 — clean at 0, sat to taste up to 1.
    ParamDef { id: 3,  name: b"Drive",     min: 0.0,    max: 1.0,    default: 0.0,   unit: ""   },
    // 0 = Off (clean), 1 = Tanh (soft), 2 = Tape (algebraic), 3 = Tube (asymmetric)
    ParamDef { id: 4,  name: b"DrvType",   min: 0.0,    max: 3.0,    default: 1.0,   unit: ""   },
    // LFO Rate in Hz (free-running) — overridden by Sync + Div when Sync is on.
    ParamDef { id: 5,  name: b"LFO Rate",  min: 0.05,   max: 20.0,   default: 0.5,   unit: "Hz" },
    // LFO Depth = how many SEMITONES of cutoff modulation per LFO unit
    // (±60 ST = ±5 octaves at the extreme). 0 = LFO disabled.
    ParamDef { id: 6,  name: b"LFO Dpt",   min: -60.0,  max: 60.0,   default: 0.0,   unit: "ST" },
    // LFO Shape: 0 = Sine, 1 = Tri, 2 = Saw, 3 = Square
    ParamDef { id: 7,  name: b"LFO Shp",   min: 0.0,    max: 3.0,    default: 0.0,   unit: ""   },
    // LFO Sync: 0 = Free (Hz), 1 = Sync (host BPM × Div)
    ParamDef { id: 8,  name: b"LFO Sync",  min: 0.0,    max: 1.0,    default: 0.0,   unit: ""   },
    // LFO Div — musical division index 0..16, mapped via synth-core's
    // sync_division_hz helper (1/1 down to 1/16 with dotted + triplet variants).
    ParamDef { id: 9,  name: b"LFO Div",   min: 0.0,    max: 16.0,   default: 6.0,   unit: ""   },
    // Envelope follower depth in semitones — input level lifts cutoff
    // for auto-wah / envelope-followed filter sweeps. ±60 ST range.
    ParamDef { id: 10, name: b"Env Dpt",   min: -60.0,  max: 60.0,   default: 0.0,   unit: "ST" },
    ParamDef { id: 11, name: b"Env Atk",   min: 0.1,    max: 200.0,  default: 5.0,   unit: "ms" },
    ParamDef { id: 12, name: b"Env Rel",   min: 5.0,    max: 1000.0, default: 120.0, unit: "ms" },
    // Dry/wet mix
    ParamDef { id: 13, name: b"Mix",       min: 0.0,    max: 1.0,    default: 1.0,   unit: ""   },
    ParamDef { id: 14, name: b"Output",    min: -24.0,  max: 12.0,   default: 0.0,   unit: "dB" },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[0, 4, 7, 8, 9];

pub const P_TYPE: usize = 0;
pub const P_CUTOFF: usize = 1;
pub const P_RESO: usize = 2;
pub const P_DRIVE: usize = 3;
pub const P_DRV_TYPE: usize = 4;
pub const P_LFO_RATE: usize = 5;
pub const P_LFO_DPT: usize = 6;
pub const P_LFO_SHP: usize = 7;
pub const P_LFO_SYNC: usize = 8;
pub const P_LFO_DIV: usize = 9;
pub const P_ENV_DPT: usize = 10;
pub const P_ENV_ATK: usize = 11;
pub const P_ENV_REL: usize = 12;
pub const P_MIX: usize = 13;
pub const P_OUTPUT: usize = 14;

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
    /// Currently-selected preset index — persisted via simple_state
    /// so the dropdown survives project reopens.
    pub active_preset: std::sync::atomic::AtomicU32,
    /// Host BPM updated from Transport events, used by Sync mode.
    pub host_bpm: AtomicF32,
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
                active_preset: std::sync::atomic::AtomicU32::new(0),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
                host_bpm: AtomicF32::new(120.0),
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
// Helpers
// ---------------------------------------------------------------------------

/// Map the P_CUTOFF "MIDI-style" param (0..127) to Hz, log-spaced
/// from 20 Hz to 20 kHz so the slider feels musical (one octave
/// per ~18 units). Same mapping as the Sampler filter for muscle-
/// memory consistency.
#[inline]
pub fn cutoff_units_to_hz(v: f32) -> f32 {
    let v = v.clamp(0.0, 127.0) / 127.0;
    20.0 * 1000f32.powf(v)
}

#[inline]
fn filter_mode_from_param(v: f32) -> SvfMode {
    match v.round() as i32 {
        1 => SvfMode::Hp,
        2 => SvfMode::Bp,
        3 => SvfMode::Notch,
        _ => SvfMode::Lp,
    }
}

#[inline]
fn lfo_shape(shape_idx: u32, phase: f32) -> f32 {
    let p = phase.fract();
    match shape_idx {
        1 => 4.0 * (p - 0.5).abs() - 1.0,         // triangle: -1..+1
        2 => 2.0 * p - 1.0,                        // saw: -1..+1
        3 => if p < 0.5 { 1.0 } else { -1.0 },     // square: -1..+1
        _ => (p * core::f32::consts::TAU).sin(),  // sine
    }
}

#[inline]
fn apply_drive(curve_idx: u32, x: f32, drive: f32) -> f32 {
    if drive < 1e-4 { return x; }
    match curve_idx {
        1 => tanh_drive(x, drive),
        2 => tape_clip(x, drive),
        3 => tube_clip(x, drive),
        _ => x,
    }
}

// ---------------------------------------------------------------------------
// Audio processor
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    filter_l: SvfFilter,
    filter_r: SvfFilter,
    env: EnvelopeDetector,
    /// LFO phase in [0, 1).
    lfo_phase: f32,
    smooth_cutoff: SmoothedParam,
    smooth_reso: SmoothedParam,
    smooth_drive: SmoothedParam,
    smooth_lfo_rate: SmoothedParam,
    smooth_lfo_dpt: SmoothedParam,
    smooth_env_dpt: SmoothedParam,
    smooth_mix: SmoothedParam,
    smooth_output: SmoothedParam,
    sample_rate: f32,
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
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
        let sr = cfg.sample_rate as f32;
        slog!("activate sr={}", sr);
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        Ok(Self {
            shared,
            filter_l: SvfFilter::default(),
            filter_r: SvfFilter::default(),
            env: EnvelopeDetector::default(),
            lfo_phase: 0.0,
            smooth_cutoff: SmoothedParam::new(load(P_CUTOFF)),
            smooth_reso: SmoothedParam::new(load(P_RESO)),
            smooth_drive: SmoothedParam::new(load(P_DRIVE)),
            smooth_lfo_rate: SmoothedParam::new(load(P_LFO_RATE)),
            smooth_lfo_dpt: SmoothedParam::new(load(P_LFO_DPT)),
            smooth_env_dpt: SmoothedParam::new(load(P_ENV_DPT)),
            smooth_mix: SmoothedParam::new(load(P_MIX)),
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
        // Flush denormals to zero — long decays / feedback loops
        // otherwise generate ≈10⁻³⁸ floats that murder CPU and cause
        // periodic ticks at the buffer rate. RAII restores host CSR.
        let _denormals = superduper_dsp_sdk::denormals::Guard::new();
        apply_param_events(self.shared, events.input);
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(
            &self.shared.params, &self.shared.dirty_params, events.output);
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(
            &self.shared.gesture_begin, &self.shared.gesture_end, events.output);

        // Catch host BPM updates for Sync mode.
        for batch in events.input.batch() {
            for ev in batch.events() {
                if let Some(CoreEventSpace::Transport(t)) = ev.as_core_event() {
                    self.shared.host_bpm.store(t.tempo as f32, Ordering::Relaxed);
                }
            }
        }

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        let sr = self.sample_rate;
        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let mode = filter_mode_from_param(load(P_TYPE));
        let drv_type = load(P_DRV_TYPE).round() as u32;
        let lfo_shp = load(P_LFO_SHP).round() as u32;
        let lfo_sync_on = load(P_LFO_SYNC) >= 0.5;
        let lfo_div = load(P_LFO_DIV).round() as u32;
        let env_atk_ms = load(P_ENV_ATK);
        let env_rel_ms = load(P_ENV_REL);
        let bpm = self.shared.host_bpm.load(Ordering::Relaxed);

        let cutoff_t = load(P_CUTOFF);
        let reso_t = load(P_RESO);
        let drive_t = load(P_DRIVE);
        let lfo_rate_t = load(P_LFO_RATE);
        let lfo_dpt_t = load(P_LFO_DPT);
        let env_dpt_t = load(P_ENV_DPT);
        let mix_t = load(P_MIX);
        let output_t = load(P_OUTPUT);

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            let mut iter = channel_pairs.into_iter();
            let Some(ch_l) = iter.next() else { continue };
            let ch_r = iter.next();
            use superduper_dsp_sdk::clap_helpers::split_io;
            let Some((l_read, l_write)) = split_io(ch_l) else { continue };
            let r = ch_r.and_then(split_io);

            if bypassed {
                l_write.copy_from_slice(l_read);
                if let Some((r_read, r_write)) = r { r_write.copy_from_slice(r_read); }
                continue;
            }

            let n = l_write.len();
            // r_write might be present or absent (mono)
            match r {
                Some((r_read, r_write)) => {
                    let n = n.min(r_read.len());
                    for i in 0..n {
                        let cutoff_u = self.smooth_cutoff.step(cutoff_t, sr);
                        let reso = self.smooth_reso.step(reso_t, sr).clamp(0.0, 0.97);
                        let drive = self.smooth_drive.step(drive_t, sr).clamp(0.0, 1.0);
                        let lfo_rate = self.smooth_lfo_rate.step(lfo_rate_t, sr);
                        let lfo_dpt = self.smooth_lfo_dpt.step(lfo_dpt_t, sr);
                        let env_dpt = self.smooth_env_dpt.step(env_dpt_t, sr);
                        let mix = self.smooth_mix.step(mix_t, sr).clamp(0.0, 1.0);
                        let out_lin = 10f32.powf(self.smooth_output.step(output_t, sr) / 20.0);

                        // LFO advance — Hz directly or computed from BPM × Div.
                        let rate_hz = if lfo_sync_on {
                            sync_division_hz(lfo_div, bpm)
                        } else { lfo_rate };
                        self.lfo_phase += rate_hz / sr;
                        if self.lfo_phase >= 1.0 { self.lfo_phase -= 1.0; }
                        let lfo = lfo_shape(lfo_shp, self.lfo_phase);
                        let lfo_st = lfo * lfo_dpt;

                        // Envelope follower — average of |L| and |R|.
                        let dry_l = l_read[i];
                        let dry_r = r_read[i];
                        let sc = (dry_l.abs() + dry_r.abs()) * 0.5;
                        let env_lvl = self.env.process(sc, sr, env_atk_ms, env_rel_ms);
                        let env_st = env_lvl.clamp(0.0, 1.0) * env_dpt;

                        let base_hz = cutoff_units_to_hz(cutoff_u);
                        let mod_hz = base_hz * 2f32.powf((lfo_st + env_st) / 12.0);
                        let cutoff_hz = mod_hz.clamp(20.0, sr * 0.49);

                        let pre_l = apply_drive(drv_type, dry_l, drive);
                        let pre_r = apply_drive(drv_type, dry_r, drive);
                        let wet_l = self.filter_l.process(pre_l, mode, cutoff_hz, reso, sr);
                        let wet_r = self.filter_r.process(pre_r, mode, cutoff_hz, reso, sr);
                        let out_l = (dry_l * (1.0 - mix) + wet_l * mix) * out_lin;
                        let out_r = (dry_r * (1.0 - mix) + wet_r * mix) * out_lin;
                        l_write[i] = out_l;
                        r_write[i] = out_r;
                        self.shared.scope.push((out_l + out_r) * 0.5);
                    }
                }
                None => {
                    for i in 0..n {
                        let cutoff_u = self.smooth_cutoff.step(cutoff_t, sr);
                        let reso = self.smooth_reso.step(reso_t, sr).clamp(0.0, 0.97);
                        let drive = self.smooth_drive.step(drive_t, sr).clamp(0.0, 1.0);
                        let lfo_rate = self.smooth_lfo_rate.step(lfo_rate_t, sr);
                        let lfo_dpt = self.smooth_lfo_dpt.step(lfo_dpt_t, sr);
                        let env_dpt = self.smooth_env_dpt.step(env_dpt_t, sr);
                        let mix = self.smooth_mix.step(mix_t, sr).clamp(0.0, 1.0);
                        let out_lin = 10f32.powf(self.smooth_output.step(output_t, sr) / 20.0);
                        let rate_hz = if lfo_sync_on {
                            sync_division_hz(lfo_div, bpm)
                        } else { lfo_rate };
                        self.lfo_phase += rate_hz / sr;
                        if self.lfo_phase >= 1.0 { self.lfo_phase -= 1.0; }
                        let lfo = lfo_shape(lfo_shp, self.lfo_phase);
                        let lfo_st = lfo * lfo_dpt;
                        let dry = l_read[i];
                        let env_lvl = self.env.process(dry.abs(), sr, env_atk_ms, env_rel_ms);
                        let env_st = env_lvl.clamp(0.0, 1.0) * env_dpt;
                        let base_hz = cutoff_units_to_hz(cutoff_u);
                        let cutoff_hz = (base_hz * 2f32.powf((lfo_st + env_st) / 12.0))
                            .clamp(20.0, sr * 0.49);
                        let pre = apply_drive(drv_type, dry, drive);
                        let wet = self.filter_l.process(pre, mode, cutoff_hz, reso, sr);
                        let out = (dry * (1.0 - mix) + wet * mix) * out_lin;
                        l_write[i] = out;
                        self.shared.scope.push(out);
                    }
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
    fn count(&mut self, is_input: bool) -> u32 { if is_input { 1 } else { 1 } }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index != 0 { return; }
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: if is_input { b"Input" } else { b"Output" },
            channel_count: 2,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            in_place_pair: Some(ClapId::new(0)),
        });
    }
}

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 { PARAMS.len() as u32 }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info_stepped(PARAMS, idx, info, STEPPED_PARAMS);
    }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        let i = id.get() as usize;
        self.shared.params.get(i).map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(&mut self, id: ClapId, v: f64, w: &mut ParamDisplayWriter) -> core::fmt::Result {
        // Special-case the LFO Div param so it shows musical labels
        // ("1/4", "1/8t", …) instead of a raw integer.
        if id.get() as usize == P_LFO_DIV {
            use core::fmt::Write;
            let idx = (v.round() as i32).max(0) as u32;
            return write!(w, "{}", sync_division_label(idx));
        }
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

superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);

// ---------------------------------------------------------------------------
// CLAP GUI extension
// ---------------------------------------------------------------------------

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
    fn create(&mut self, _: GuiConfiguration) -> Result<(), PluginError> { Ok(()) }
    fn destroy(&mut self) { self.gui_handle = None; }
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

pub struct SuperDuperFilter;

impl Plugin for SuperDuperFilter {
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

impl DefaultPluginFactory for SuperDuperFilter {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.filter",
            plugin_display_name!("SuperDuper Filter"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Multi-mode resonant filter with drive, LFO and envelope follower")
        .with_features([AUDIO_EFFECT, STEREO, FILTER])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Filter — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperFilter>);
