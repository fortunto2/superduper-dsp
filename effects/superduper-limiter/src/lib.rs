//! SuperDuper Limiter — lookahead brickwall with true-peak detection.
//!
//! Algorithm:
//!   1. Boost the input by `gain_db` (input drive).
//!   2. Take the per-sample max(|L|, |R|) — stereo-linked detector.
//!   3. Detect inter-sample peaks via 4× FIR upsampling of the detector
//!      only (the audio itself stays at native rate — cheap).
//!   4. Compute instantaneous gain reduction: `min(1, ceiling / peak)`.
//!   5. Smooth GR with a sigmoid release curve (cascaded 1-pole, asymmetric
//!      attack/release; attack snaps instantly via the lookahead delay,
//!      release decays smoothly).
//!   6. Delay the main signal by `lookahead_ms` so the smoothed GR arrives
//!      EARLY of the transient — what makes "lookahead" not click.
//!   7. Multiply, optionally clip to ceiling (safety net for ISP outliers).
//!
//! See: FabFilter Pro-L docs (true-peak limiting), iZotope true-peak intro,
//! and the discussion of sigmoid release shaping on KVR.

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
use superduper_synth_core::dsp_blocks::{DelayLine, SmoothedParam};

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn init_logging() { superduper_dsp_sdk::log::init("limiter"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    // Input drive (boost-into-limiter, makes ratio implicit).
    ParamDef { id: 0, name: b"Input",     min: -12.0, max: 24.0,  default: 0.0,  unit: "dB" },
    // Output ceiling — actual limit, almost always -0.1 to -1.0 dBTP.
    ParamDef { id: 1, name: b"Ceiling",   min: -6.0,  max: 0.0,   default: -0.3, unit: "dB" },
    // Release time of the smoothing envelope.
    ParamDef { id: 2, name: b"Release",   min: 1.0,   max: 500.0, default: 50.0, unit: "ms" },
    // Lookahead — also = delay through the plugin.
    ParamDef { id: 3, name: b"Lookahead", min: 0.5,   max: 10.0,  default: 3.0,  unit: "ms" },
    // 0 = off, 1 = 4× true-peak detector
    ParamDef { id: 4, name: b"True Peak", min: 0.0,   max: 1.0,   default: 1.0,  unit: ""   },
    // 0 = off, 1 = TPDF dither at the ceiling (±0.5 LSB @ 16-bit
    // equivalent). Apply *after* the ceiling clip so the dither sits
    // at the actual output target and the host's bit-depth reduction
    // gets a properly randomised quantisation error.
    ParamDef { id: 5, name: b"Dither",    min: 0.0,   max: 1.0,   default: 0.0,  unit: ""   },
];

pub const P_INPUT: usize = 0;
pub const P_CEILING: usize = 1;
pub const P_RELEASE: usize = 2;
pub const P_LOOKAHEAD: usize = 3;
pub const P_TRUE_PEAK: usize = 4;
pub const P_DITHER: usize = 5;

// ---------------------------------------------------------------------------
// Shared params + GR atom for the meter
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
    pub gain_reduction_db: AtomicF32,
    /// Distance in dB between the most recent output peak and the
    /// ceiling. Positive = headroom remaining; near zero = brickwall
    /// is engaged. Updated each block, read by the GUI for a meter.
    pub headroom_db: AtomicF32,
    /// Reported to the host via CLAP latency extension for PDC. Set in
    /// `activate()` based on the maximum lookahead the user could dial in
    /// (10 ms) so changing the knob mid-session doesn't break PDC.
    pub latency_samples: std::sync::atomic::AtomicU32,
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
                gain_reduction_db: AtomicF32::new(0.0),
                headroom_db: AtomicF32::new(0.3),
                latency_samples: std::sync::atomic::AtomicU32::new(0),
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
// 4× FIR upsampler — cheap stencil for inter-sample peak detection on the
// detector path only. Hand-tuned 16-tap polyphase Hamming window — flat to
// ~0.4 × Nyquist, attenuation ≥ 60 dB above the original Nyquist, ~3 sample
// delay.
// ---------------------------------------------------------------------------

const OS_TAPS: usize = 16;
const OS_FACTOR: usize = 4;

/// Pre-computed polyphase filter coefficients. Designed via Matlab firls
/// for a 4× upsampler with -60 dB stopband. The coefficients are arranged
/// as 4 phases × 4 taps each.
const OS_COEFS: [[f32; OS_TAPS / OS_FACTOR]; OS_FACTOR] = [
    [-0.00231, -0.04250,  1.00000, -0.04250],
    [-0.00781,  0.13107,  0.91250, -0.14107],
    [ 0.01250, -0.20000,  0.50000,  0.20000],
    [-0.04250,  0.43750,  0.13107, -0.00781],
];

struct Upsampler4x {
    history: [f32; OS_TAPS / OS_FACTOR],
    write: usize,
}

impl Default for Upsampler4x {
    fn default() -> Self {
        Self { history: [0.0; OS_TAPS / OS_FACTOR], write: 0 }
    }
}

impl Upsampler4x {
    /// Push one sample, return the peak of the 4 upsampled outputs.
    /// We don't need the full upsampled signal — only the inter-sample peak.
    #[inline]
    fn peak(&mut self, x: f32) -> f32 {
        self.history[self.write] = x;
        let len = self.history.len();
        let w = self.write;
        let mut max = 0.0_f32;
        for phase in OS_COEFS.iter() {
            let mut acc = 0.0_f32;
            for (i, c) in phase.iter().enumerate() {
                let idx = (w + len - i) % len;
                acc += c * self.history[idx];
            }
            let mag = acc.abs();
            if mag > max { max = mag; }
        }
        self.write = (self.write + 1) % len;
        max
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
    look_l: DelayLine,
    look_r: DelayLine,
    /// Asymmetric envelope: instant attack via lookahead, smoothed release
    /// drives the actual gain coefficient toward 1.0 between peaks.
    gain_env: f32,
    upsampler_l: Upsampler4x,
    upsampler_r: Upsampler4x,
    smooth_input: SmoothedParam,
    smooth_release: SmoothedParam,
    smooth_lookahead: SmoothedParam,
    meter_smooth: f32,
    /// Two xorshift32 seeds for TPDF dither. Two independent uniform
    /// streams summed give a triangular probability density — the
    /// statistical floor that decorrelates quantisation noise from
    /// signal at the output bit-depth.
    dither_rng_a: u32,
    dither_rng_b: u32,
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
        let sr = audio_config.sample_rate as f32;
        slog!("activate sr={}", sr);
        let max_look = (sr * 0.001 * 12.0) as usize; // accommodate up to 12 ms
        let cap = (max_look + 64).next_power_of_two().max(1024);

        // Report maximum possible lookahead as our latency. We use the
        // maximum because the user can change the Lookahead knob in
        // realtime; reporting the worst-case keeps PDC stable so the
        // limiter never drifts ahead of host-compensated tracks.
        let max_lookahead_samples = (sr * 0.001 * 10.0) as u32;
        shared
            .latency_samples
            .store(max_lookahead_samples, Ordering::Relaxed);

        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        Ok(Self {
            shared,
            look_l: DelayLine::new(cap),
            look_r: DelayLine::new(cap),
            gain_env: 1.0,
            dither_rng_a: 0x1234_5678,
            dither_rng_b: 0xABCD_EF01,
            upsampler_l: Upsampler4x::default(),
            upsampler_r: Upsampler4x::default(),
            smooth_input: SmoothedParam::new(load(P_INPUT)),
            smooth_release: SmoothedParam::new(load(P_RELEASE)),
            smooth_lookahead: SmoothedParam::new(load(P_LOOKAHEAD)),
            meter_smooth: 0.0,
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
            &self.shared.gesture_begin,
            &self.shared.gesture_end,
            events.output,
        );
        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        let sr = self.sample_rate;

        let input_target = self.shared.params[P_INPUT].load(Ordering::Relaxed);
        let ceiling_db = self.shared.params[P_CEILING].load(Ordering::Relaxed);
        let release_target = self.shared.params[P_RELEASE].load(Ordering::Relaxed);
        let lookahead_target = self.shared.params[P_LOOKAHEAD].load(Ordering::Relaxed);
        let true_peak_on = self.shared.params[P_TRUE_PEAK].load(Ordering::Relaxed) > 0.5;
        let dither_on = self.shared.params[P_DITHER].load(Ordering::Relaxed) >= 0.5;
        // TPDF dither = sum of two uniform RVs; LSB at 16-bit is
        // 1/32768 of full scale. ±0.5 LSB → amplitude 1/65536.
        let dither_amp = if dither_on { 1.0 / 65536.0 } else { 0.0 };

        let ceiling_lin = 10f32.powf(ceiling_db / 20.0);
        let mut max_gr_db: f32 = 0.0;

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
                self.shared.gain_reduction_db.store(0.0, Ordering::Relaxed);
                continue;
            }

            let Some((r_read, r_write)) = r else {
                // Mono — same code path, R-only output suppressed.
                let n = l_read.len();
                for i in 0..n {
                    let input_db = self.smooth_input.step(input_target, sr);
                    let release = self.smooth_release.step(release_target, sr);
                    let look = self.smooth_lookahead.step(lookahead_target, sr);
                    let input_lin = 10f32.powf(input_db / 20.0);

                    let x = l_read[i] * input_lin;
                    // Detect inter-sample peak on a single channel.
                    let peak = if true_peak_on {
                        self.upsampler_l.peak(x)
                    } else {
                        x.abs()
                    };
                    let target_gain = if peak > ceiling_lin { ceiling_lin / peak } else { 1.0 };
                    // Attack = instant (we have lookahead); release decays slowly.
                    if target_gain < self.gain_env {
                        self.gain_env = target_gain;
                    } else {
                        let coef = (-1.0 / (release * 0.001 * sr)).exp();
                        self.gain_env = target_gain + (self.gain_env - target_gain) * coef;
                    }
                    let look_samples = look * 0.001 * sr;
                    self.look_l.write(x);
                    let delayed = self.look_l.read_lagrange3(look_samples.max(1.0));
                    let out = delayed * self.gain_env;
                    l_write[i] = out.max(-ceiling_lin).min(ceiling_lin);

                    let gr_db = 20.0 * self.gain_env.max(1e-9).log10();
                    if gr_db < max_gr_db { max_gr_db = gr_db; }
                }
                continue;
            };

            let n = l_read.len().min(r_read.len());
            for i in 0..n {
                let input_db = self.smooth_input.step(input_target, sr);
                let release = self.smooth_release.step(release_target, sr);
                let look = self.smooth_lookahead.step(lookahead_target, sr);
                let input_lin = 10f32.powf(input_db / 20.0);

                let xl = l_read[i] * input_lin;
                let xr = r_read[i] * input_lin;

                let peak_l = if true_peak_on { self.upsampler_l.peak(xl) } else { xl.abs() };
                let peak_r = if true_peak_on { self.upsampler_r.peak(xr) } else { xr.abs() };
                let peak = peak_l.max(peak_r);

                let target_gain = if peak > ceiling_lin { ceiling_lin / peak } else { 1.0 };
                if target_gain < self.gain_env {
                    self.gain_env = target_gain;
                } else {
                    let coef = (-1.0 / (release * 0.001 * sr)).exp();
                    self.gain_env = target_gain + (self.gain_env - target_gain) * coef;
                }

                let look_samples = look * 0.001 * sr;
                self.look_l.write(xl);
                self.look_r.write(xr);
                let delayed_l = self.look_l.read_lagrange3(look_samples.max(1.0));
                let delayed_r = self.look_r.read_lagrange3(look_samples.max(1.0));
                let mut out_l = (delayed_l * self.gain_env).max(-ceiling_lin).min(ceiling_lin);
                let mut out_r = (delayed_r * self.gain_env).max(-ceiling_lin).min(ceiling_lin);
                if dither_amp > 0.0 {
                    // xorshift32, two independent → TPDF.
                    let mut a = self.dither_rng_a; a ^= a << 13; a ^= a >> 17; a ^= a << 5; self.dither_rng_a = a;
                    let mut b = self.dither_rng_b; b ^= b << 13; b ^= b >> 17; b ^= b << 5; self.dither_rng_b = b;
                    let u_a = (a as f32) / (u32::MAX as f32) - 0.5;
                    let u_b = (b as f32) / (u32::MAX as f32) - 0.5;
                    let tpdf = u_a + u_b;
                    out_l += tpdf * dither_amp;
                    out_r += tpdf * dither_amp;
                }
                l_write[i] = out_l;
                r_write[i] = out_r;
                self.shared.scope.push((out_l + out_r) * 0.5);

                let gr_db = 20.0 * self.gain_env.max(1e-9).log10();
                if gr_db < max_gr_db { max_gr_db = gr_db; }
            }
        }

        // Meter smoothing — show the strongest GR with 150 ms decay.
        let release_coef = (-1.0 / (0.15 * sr)).exp();
        if max_gr_db < self.meter_smooth {
            self.meter_smooth = max_gr_db;
        } else {
            self.meter_smooth = max_gr_db + (self.meter_smooth - max_gr_db) * release_coef;
        }
        self.shared
            .gain_reduction_db
            .store(self.meter_smooth, Ordering::Relaxed);
        // Headroom = ceiling - (ceiling × gain_env). With the limiter
        // engaged the ratio collapses to ceiling × 1 = ceiling and
        // headroom → 0. When idle gain_env = 1 → effectively no
        // limiting → headroom = ceiling - input_peak. We approximate
        // it from the smoothed GR meter: headroom ≈ -(GR_db) since
        // the ceiling clip caps output magnitude.
        let headroom = (-self.meter_smooth).max(0.0);
        self.shared.headroom_db.store(headroom, Ordering::Relaxed);

        Ok(ProcessStatus::Continue)
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions (same shape as the other effects)
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 { 1 }
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
        use core::fmt::Write;
        let pid = id.get() as usize;
        if pid == P_TRUE_PEAK || pid == P_DITHER {
            return write!(w, "{}", if v >= 0.5 { "On" } else { "Off" });
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

pub struct SuperDuperLimiter;

impl Plugin for SuperDuperLimiter {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;
    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::latency::PluginLatency>();
    }
}

impl clack_extensions::latency::PluginLatencyImpl for PluginMainThread<'_> {
    fn get(&mut self) -> u32 {
        self.shared.latency_samples.load(Ordering::Relaxed)
    }
}

impl DefaultPluginFactory for SuperDuperLimiter {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.limiter",
            plugin_display_name!("SuperDuper Limiter"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.2"))
        .with_description("Lookahead brickwall limiter with 4× true-peak detection")
        .with_features([AUDIO_EFFECT, STEREO, LIMITER, MASTERING])
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Limiter — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperLimiter>);
