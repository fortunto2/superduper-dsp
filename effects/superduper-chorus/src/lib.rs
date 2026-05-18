//! SuperDuper Chorus — stereo BBD-style chorus.
//!
//! Two delay lines per channel (L & R), each modulated by a sine LFO
//! 90° out of phase between L and R for the classic "wide chorus"
//! image. Delay time sits in the 5-25 ms range — classic BBD territory.
//! Lagrange-3 fractional taps keep the pitch-mod smooth (linear interp
//! introduces a 6 dB high-shelf droop at Nyquist when sweeping the tap
//! point, which kills the silky chorus shimmer).
//!
//! Architecture per channel:
//!
//! ```text
//!   in_L → delay_L(time + lfo_L * depth) ──┐
//!                                          ├─→ tap_L → wet_L
//!                                          │
//!                                       feedback ←── × feedback gain
//!                                          │
//!   write into delay_L: in_L + feedback   ──┘
//! ```
//!
//! Spread param widens or narrows the L/R LFO phase relationship —
//! at 0 both LFOs are in phase (mono chorus), at 1 they're 90° apart.
//!
//! Mix blends dry + wet; full-wet is rare (the modulation produces
//! noticeable pitch drift) — 30-60% is the lush sweet spot.

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
use superduper_synth_core::dsp_blocks::{DcBlocker, DelayLine, SmoothedParam};

// ---------------------------------------------------------------------------
// Logging — minimal, behind a file mutex so it doesn't risk RT-thread writes.
// ---------------------------------------------------------------------------

fn log_path() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".superduper-dsp")
        .join("chorus.log")
}

static LOG_FILE: std::sync::OnceLock<parking_lot::Mutex<Option<std::fs::File>>> =
    std::sync::OnceLock::new();

fn init_logging() {
    LOG_FILE.get_or_init(|| {
        let path = log_path();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        parking_lot::Mutex::new(file)
    });
}

fn slog_args(args: std::fmt::Arguments<'_>) {
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

macro_rules! slog { ($($arg:tt)*) => { $crate::slog_args(format_args!($($arg)*)) } }

// ---------------------------------------------------------------------------
// Param table — 7 controls.
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    // LFO sweep rate in Hz. 0.1 = ultra-slow tape wow, 6 = vibrato-ish.
    ParamDef { id: 0, name: b"Rate",     min: 0.05, max: 6.0,   default: 0.6,  unit: "Hz" },
    // Modulation depth — how much the LFO shifts the delay tap.
    // 0..1 maps to ±depth_ms inside the inner loop.
    ParamDef { id: 1, name: b"Depth",    min: 0.0,  max: 1.0,   default: 0.5,  unit: ""   },
    // Centre delay time in ms. BBD chorus territory = 5-25 ms.
    ParamDef { id: 2, name: b"Time",     min: 3.0,  max: 30.0,  default: 12.0, unit: "ms" },
    // Spread — L/R LFO phase relationship. 0 = mono chorus, 1 = full
    // quadrature for max stereo width.
    ParamDef { id: 3, name: b"Spread",   min: 0.0,  max: 1.0,   default: 1.0,  unit: ""   },
    // Width post-process: stereo wet-only image. 1 = full stereo,
    // 0 = mono-summed.
    ParamDef { id: 4, name: b"Width",    min: 0.0,  max: 1.0,   default: 1.0,  unit: ""   },
    // Feedback through the delay lines for thicker, more swirly tone.
    // Capped at 0.9 — beyond that you get a self-oscillating chorus
    // that turns into a clean flanger / phaser.
    ParamDef { id: 5, name: b"Feedback", min: 0.0,  max: 0.9,   default: 0.0,  unit: ""   },
    ParamDef { id: 6, name: b"Mix",      min: 0.0,  max: 1.0,   default: 0.5,  unit: ""   },
];

pub const P_RATE: usize = 0;
pub const P_DEPTH: usize = 1;
pub const P_TIME: usize = 2;
pub const P_SPREAD: usize = 3;
pub const P_WIDTH: usize = 4;
pub const P_FEEDBACK: usize = 5;
pub const P_MIX: usize = 6;

// ---------------------------------------------------------------------------
// Shared params + scope
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
    delay_l: DelayLine,
    delay_r: DelayLine,
    /// Two free-running LFOs (one per channel, phase offset by Spread).
    lfo_l_phase: f32,
    lfo_r_phase: f32,
    /// Per-channel feedback storage so we can write `in + fb*prev` into
    /// the delay line each sample.
    fb_l: f32,
    fb_r: f32,
    /// DC blocker on the wet path so feedback can't accumulate offset.
    dc_l: DcBlocker,
    dc_r: DcBlocker,
    // Smoothed params (slewed on the audio thread to kill zipper noise).
    smooth_rate: SmoothedParam,
    smooth_depth: SmoothedParam,
    smooth_time: SmoothedParam,
    smooth_spread: SmoothedParam,
    smooth_width: SmoothedParam,
    smooth_feedback: SmoothedParam,
    smooth_mix: SmoothedParam,
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
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        init_logging();
        let sr = audio_config.sample_rate as f32;
        slog!("chorus activate sr={}", sr);
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        // Maximum delay capacity = centre + depth swing + lookahead margin.
        // 30 ms centre + 25 ms swing = ~55 ms; round to 64 ms for safety.
        let max_delay = (sr * 0.001 * 64.0).ceil() as usize;
        Ok(Self {
            shared,
            delay_l: DelayLine::new(max_delay),
            delay_r: DelayLine::new(max_delay),
            lfo_l_phase: 0.0,
            lfo_r_phase: 0.0,
            fb_l: 0.0,
            fb_r: 0.0,
            dc_l: DcBlocker::default(),
            dc_r: DcBlocker::default(),
            smooth_rate: SmoothedParam::new(load(P_RATE)),
            smooth_depth: SmoothedParam::new(load(P_DEPTH)),
            smooth_time: SmoothedParam::new(load(P_TIME)),
            smooth_spread: SmoothedParam::new(load(P_SPREAD)),
            smooth_width: SmoothedParam::new(load(P_WIDTH)),
            smooth_feedback: SmoothedParam::new(load(P_FEEDBACK)),
            smooth_mix: SmoothedParam::new(load(P_MIX)),
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

        let rate_t = self.shared.params[P_RATE].load(Ordering::Relaxed);
        let depth_t = self.shared.params[P_DEPTH].load(Ordering::Relaxed);
        let time_t = self.shared.params[P_TIME].load(Ordering::Relaxed);
        let spread_t = self.shared.params[P_SPREAD].load(Ordering::Relaxed);
        let width_t = self.shared.params[P_WIDTH].load(Ordering::Relaxed);
        let fb_t = self.shared.params[P_FEEDBACK].load(Ordering::Relaxed);
        let mix_t = self.shared.params[P_MIX].load(Ordering::Relaxed);

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
                if let Some((rr, rw)) = r { rw.copy_from_slice(rr); }
                continue;
            }

            match r {
                Some((r_read, r_write)) => {
                    stereo_process(self, l_read, l_write, r_read, r_write, sr,
                        rate_t, depth_t, time_t, spread_t, width_t, fb_t, mix_t);
                }
                None => {
                    // Mono input: duplicate to both delay lines and write the
                    // L-channel wet (R-side stays effectively silent on a
                    // mono port).
                    mono_process(self, l_read, l_write, sr,
                        rate_t, depth_t, time_t, fb_t, mix_t);
                }
            }
        }

        Ok(ProcessStatus::Continue)
    }
}

#[inline]
fn lfo_sample(phase: f32) -> f32 {
    (phase * core::f32::consts::TAU).sin()
}

#[allow(clippy::too_many_arguments)]
fn stereo_process(
    p: &mut PluginAudioProcessor<'_>,
    l_read: &[f32], l_write: &mut [f32],
    r_read: &[f32], r_write: &mut [f32],
    sr: f32,
    rate_t: f32, depth_t: f32, time_t: f32,
    spread_t: f32, width_t: f32, fb_t: f32, mix_t: f32,
) {
    let n = l_read.len().min(r_read.len());
    // Max LFO swing in ms — depth=1 → ±9 ms peak excursion. Combined
    // with a 12 ms centre that's a 3–21 ms tap range, fully inside
    // the BBD chorus sweet spot.
    const MAX_DEPTH_MS: f32 = 9.0;

    for i in 0..n {
        let rate = p.smooth_rate.step(rate_t, sr).max(0.01);
        let depth = p.smooth_depth.step(depth_t, sr).clamp(0.0, 1.0);
        let time_ms = p.smooth_time.step(time_t, sr).max(1.0);
        let spread = p.smooth_spread.step(spread_t, sr).clamp(0.0, 1.0);
        let width = p.smooth_width.step(width_t, sr).clamp(0.0, 1.0);
        let fb = p.smooth_feedback.step(fb_t, sr).clamp(0.0, 0.9);
        let mix = p.smooth_mix.step(mix_t, sr).clamp(0.0, 1.0);

        // Advance the two LFOs. Spread maps to L/R phase offset (0 =
        // in-phase mono, 1 = full quadrature 90°). R lags L by that
        // amount — feels natural to most ears than leading.
        p.lfo_l_phase += rate / sr;
        if p.lfo_l_phase >= 1.0 { p.lfo_l_phase -= 1.0; }
        let phase_offset = 0.25 * spread;       // 0..0.25 cycle
        p.lfo_r_phase = (p.lfo_l_phase + phase_offset).fract();

        let mod_l = lfo_sample(p.lfo_l_phase) * depth * MAX_DEPTH_MS;
        let mod_r = lfo_sample(p.lfo_r_phase) * depth * MAX_DEPTH_MS;

        let tap_l_ms = (time_ms + mod_l).max(0.5);
        let tap_r_ms = (time_ms + mod_r).max(0.5);
        let tap_l_samples = tap_l_ms * 0.001 * sr;
        let tap_r_samples = tap_r_ms * 0.001 * sr;

        let dry_l = l_read[i];
        let dry_r = r_read[i];

        // Write input + feedback into the delay lines BEFORE reading
        // the tap — gives us the standard one-sample loop and lets
        // feedback recirculate without a hard zero-delay path.
        p.delay_l.write(dry_l + p.fb_l * fb);
        p.delay_r.write(dry_r + p.fb_r * fb);

        let mut wet_l = p.delay_l.read_lagrange3(tap_l_samples);
        let mut wet_r = p.delay_r.read_lagrange3(tap_r_samples);

        // DC-block the feedback path so accumulating offset never
        // wanders the tail away from zero.
        wet_l = p.dc_l.process(wet_l);
        wet_r = p.dc_r.process(wet_r);
        p.fb_l = wet_l;
        p.fb_r = wet_r;

        // Width post-process — narrow wet image toward mono.
        let mono_w = (wet_l + wet_r) * 0.5;
        let final_wet_l = wet_l * width + mono_w * (1.0 - width);
        let final_wet_r = wet_r * width + mono_w * (1.0 - width);

        let out_l = dry_l * (1.0 - mix) + final_wet_l * mix;
        let out_r = dry_r * (1.0 - mix) + final_wet_r * mix;
        l_write[i] = out_l;
        r_write[i] = out_r;
        p.shared.scope.push((out_l + out_r) * 0.5);
    }
}

#[allow(clippy::too_many_arguments)]
fn mono_process(
    p: &mut PluginAudioProcessor<'_>,
    l_read: &[f32], l_write: &mut [f32],
    sr: f32,
    rate_t: f32, depth_t: f32, time_t: f32,
    fb_t: f32, mix_t: f32,
) {
    const MAX_DEPTH_MS: f32 = 9.0;
    for i in 0..l_read.len() {
        let rate = p.smooth_rate.step(rate_t, sr).max(0.01);
        let depth = p.smooth_depth.step(depth_t, sr).clamp(0.0, 1.0);
        let time_ms = p.smooth_time.step(time_t, sr).max(1.0);
        let fb = p.smooth_feedback.step(fb_t, sr).clamp(0.0, 0.9);
        let mix = p.smooth_mix.step(mix_t, sr).clamp(0.0, 1.0);

        p.lfo_l_phase += rate / sr;
        if p.lfo_l_phase >= 1.0 { p.lfo_l_phase -= 1.0; }
        let mod_l = lfo_sample(p.lfo_l_phase) * depth * MAX_DEPTH_MS;
        let tap_l_samples = (time_ms + mod_l).max(0.5) * 0.001 * sr;

        let dry = l_read[i];
        p.delay_l.write(dry + p.fb_l * fb);
        let wet = p.dc_l.process(p.delay_l.read_lagrange3(tap_l_samples));
        p.fb_l = wet;
        let out = dry * (1.0 - mix) + wet * mix;
        l_write[i] = out;
        p.shared.scope.push(out);
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

// CLAP state — params + bypass via the shared SDK helper.
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
        let api_type = if cfg!(target_os = "macos") {
            GuiApiType::COCOA
        } else if cfg!(target_os = "windows") {
            GuiApiType::WIN32
        } else {
            GuiApiType::X11
        };
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
            can_resize_horizontally: true,
            can_resize_vertically: true,
            strategy: AspectRatioStrategy::Disregard,
        })
    }
    fn adjust_size(&mut self, size: GuiSize) -> Option<GuiSize> {
        Some(GuiSize {
            width: size.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH),
            height: size.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT),
        })
    }
    fn set_size(&mut self, size: GuiSize) -> Result<(), PluginError> {
        let w = size.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH);
        let h = size.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT);
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

// ---------------------------------------------------------------------------
// Factory descriptor + entry
// ---------------------------------------------------------------------------

pub struct SuperDuperChorus;

impl Plugin for SuperDuperChorus {
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

impl DefaultPluginFactory for SuperDuperChorus {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("co.superduperai.chorus", plugin_display_name!("SuperDuper Chorus"))
            .with_vendor("SuperDuperAI")
            .with_version(version_string!("0.2"))
            .with_description("Stereo BBD-style chorus — lush, post-punk, Joy Division on tap.")
            .with_features([AUDIO_EFFECT, STEREO, CHORUS])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Chorus — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperChorus>);
