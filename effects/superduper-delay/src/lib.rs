//! SuperDuper Delay — stereo delay with proper analog character.
//!
//! Algorithm (per channel, then crossed):
//!
//! ```text
//!   in_L → delay_L(time_L) ──┐
//!                            ├→ tap_L ─→ tone_L (1-pole LP)
//!                            │            │
//!                            │            ↓
//!                            │         tape_clip (saturation)
//!                            │            │
//!                            │            ↓
//!                          feedback ←──── × feedback gain
//!                            │
//!                            ↓
//!   write into delay_L: in_L + cross_R_to_L  (cross-feedback from R tap)
//!
//!   …mirror for R.
//! ```
//!
//! Mode controls who feeds whom:
//!   - **Stereo**: in_L → delay_L, in_R → delay_R, feedback stays per-side.
//!   - **Ping-pong**: in summed mono → delay_L only; delay_L's tap feeds
//!     delay_R's input (×feedback); delay_R's tap feeds delay_L's input.
//!     Classic L-R-L-R bouncing.
//!   - **Slap**: short fixed delay on one side only (Haas-style), no feedback.
//!
//! Key DSP touches (informed by Valhalla Delay + Smith's PASP):
//!   - 3rd-order Lagrange interpolation in the delay tap (`synth_core::dsp_blocks::DelayLine`).
//!     Linear interp dulls the repeats; Lagrange-3 keeps them flat.
//!   - 2-pole slew on the Time parameter so a knob sweep produces the
//!     classic tape pitch doppler (instead of a click).
//!   - `tape_clip` lives INSIDE the feedback loop, so each repeat softens
//!     progressively — the "every echo gets warmer" character of analog
//!     gear, not a static gain × feedback ring.
//!   - One-pole LP tone control in the feedback path too, so high
//!     frequencies disappear over generations (like real tape head wear).
//!   - DC blocker on the input — without it the feedback loop accumulates
//!     offset and the tail dies into a hum.

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
use superduper_synth_core::dsp_blocks::{
    tape_clip, DcBlocker, DelayLine, Ducker, OnePoleLp, SlewLimiter2Pole, SmoothedParam,
};

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn init_logging() { superduper_dsp_sdk::log::init("delay"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Param table — 7 total (one knob fewer than supermass, six DSP + Mode)
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Time",     min: 1.0,    max: 2000.0, default: 350.0, unit: "ms" },
    // Width offsets the R-channel delay relative to L (-100..+100 ms). 0 = both equal.
    ParamDef { id: 1, name: b"Width",    min: -100.0, max: 100.0,  default: 30.0,  unit: "ms" },
    ParamDef { id: 2, name: b"Feedback", min: 0.0,    max: 0.95,   default: 0.45,  unit: ""   },
    // Tone = LP cutoff inside the feedback loop. 200 Hz = very dark, 20 kHz = bright/digital.
    ParamDef { id: 3, name: b"Tone",     min: 200.0,  max: 20000.0, default: 6500.0, unit: "Hz" },
    // Drive applies tape-style saturation IN the feedback path — each repeat is degraded.
    ParamDef { id: 4, name: b"Drive",    min: 0.0,    max: 12.0,   default: 1.0,   unit: "dB" },
    ParamDef { id: 5, name: b"Mix",      min: 0.0,    max: 1.0,    default: 0.35,  unit: ""   },
    // 0 = Stereo, 1 = Ping-Pong, 2 = Slap (Haas)
    ParamDef { id: 6, name: b"Mode",     min: 0.0,    max: 2.0,    default: 0.0,   unit: ""   },
    // Ducking — sidechain port if routed, else dry input as key.
    ParamDef { id: 7, name: b"Duck Amount",  min: 0.0,    max: 24.0,   default: 0.0,   unit: "dB" },
    ParamDef { id: 8, name: b"Duck Attack",  min: 1.0,    max: 200.0,  default: 5.0,   unit: "ms" },
    ParamDef { id: 9, name: b"Duck Release", min: 10.0,   max: 1000.0, default: 200.0, unit: "ms" },
    // Tempo sync — when on, Time is computed from host BPM × Time Div.
    // Width still operates as a ms offset on top of the synced base.
    ParamDef { id: 10, name: b"Time Sync", min: 0.0, max: 1.0,  default: 0.0, unit: "" },
    // 0 = 1/1, 1 = 1/2d, 2 = 1/2, 3 = 1/2t, 4 = 1/4, 5 = 1/4d, 6 = 1/4t,
    // 7 = 1/8, 8 = 1/8d, 9 = 1/8t, 10 = 1/16, 11 = 1/16t — same enum
    // shape as Wave LFO Div / Kubyz Mouth Div for consistency.
    ParamDef { id: 11, name: b"Time Div",  min: 0.0, max: 11.0, default: 7.0, unit: "" },
];

pub const P_TIME: usize = 0;
pub const P_WIDTH: usize = 1;
pub const P_FEEDBACK: usize = 2;
pub const P_TONE: usize = 3;
pub const P_DRIVE: usize = 4;
pub const P_MIX: usize = 5;
pub const P_MODE: usize = 6;
pub const P_DUCK_AMOUNT: usize = 7;
pub const P_DUCK_ATTACK: usize = 8;
pub const P_DUCK_RELEASE: usize = 9;
pub const P_TIME_SYNC: usize = 10;
pub const P_TIME_DIV: usize = 11;

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
    /// Host transport BPM — updated from TransportEvent so Time can run
    /// in sync mode at a musical division (1/4, 1/8 etc.).
    pub host_bpm: AtomicF32,
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
                host_bpm: AtomicF32::new(120.0),
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
// Audio processor — owns the two delay lines + per-channel filters.
// ---------------------------------------------------------------------------

const MAX_DELAY_SECONDS: f32 = 2.5;

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
    dc_l: DcBlocker,
    dc_r: DcBlocker,
    tone_l: OnePoleLp,
    tone_r: OnePoleLp,
    /// Per-channel feedback state (output of last sample, fed into next write).
    fb_l: f32,
    fb_r: f32,
    /// 2-pole slew on the Time parameter — produces tape doppler on sweep.
    slew_time_l: SlewLimiter2Pole,
    slew_time_r: SlewLimiter2Pole,
    smooth_feedback: SmoothedParam,
    smooth_tone: SmoothedParam,
    smooth_drive: SmoothedParam,
    smooth_mix: SmoothedParam,
    smooth_duck: SmoothedParam,
    ducker: Ducker,
    /// Sidechain scratch (port 1). Empty / all-zero → fall back to dry as
    /// the ducking key signal (works on plain insert use).
    sc_l: Box<[f32]>,
    sc_r: Box<[f32]>,
    sample_rate: f32,
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
    // Also capture host BPM out of the transport stream so tempo-sync
    // mode tracks tempo changes inside a block.
    for event in events {
        if let Some(core) = event.as_core_event() {
            if let clack_plugin::events::spaces::CoreEventSpace::Transport(t) = core {
                shared.host_bpm.store(t.tempo as f32, Ordering::Relaxed);
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
        slog!("activate: sr={}", audio_config.sample_rate);
        let sr = audio_config.sample_rate as f32;
        let max_samples = (sr * MAX_DELAY_SECONDS) as usize;

        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        let time_l = load(P_TIME) * 0.001 * sr;
        let time_r = (load(P_TIME) + load(P_WIDTH)).max(1.0) * 0.001 * sr;

        let max_frames = audio_config.max_frames_count as usize;
        Ok(Self {
            shared,
            delay_l: DelayLine::new(max_samples),
            delay_r: DelayLine::new(max_samples),
            dc_l: DcBlocker::default(),
            dc_r: DcBlocker::default(),
            tone_l: OnePoleLp::default(),
            tone_r: OnePoleLp::default(),
            fb_l: 0.0,
            fb_r: 0.0,
            slew_time_l: SlewLimiter2Pole::new(time_l),
            slew_time_r: SlewLimiter2Pole::new(time_r),
            smooth_feedback: SmoothedParam::new(load(P_FEEDBACK)),
            smooth_tone: SmoothedParam::new(load(P_TONE)),
            smooth_drive: SmoothedParam::new(load(P_DRIVE)),
            smooth_mix: SmoothedParam::new(load(P_MIX)),
            smooth_duck: SmoothedParam::new(load(P_DUCK_AMOUNT)),
            ducker: Ducker::default(),
            sc_l: vec![0.0; max_frames].into_boxed_slice(),
            sc_r: vec![0.0; max_frames].into_boxed_slice(),
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

        // Tempo sync: if Time Sync is on, derive ms from host BPM × Time Div
        // (musical note value). Width still operates as a ms offset on top.
        let time_sync = self.shared.params[P_TIME_SYNC]
            .load(Ordering::Relaxed) >= 0.5;
        let time_ms = if time_sync {
            let div = self.shared.params[P_TIME_DIV]
                .load(Ordering::Relaxed) as u32;
            let bpm = self.shared.host_bpm.load(Ordering::Relaxed);
            let hz = superduper_synth_core::dsp_blocks::sync_division_hz(div, bpm);
            (1000.0 / hz.max(0.5)).clamp(1.0, 2000.0)
        } else {
            self.shared.params[P_TIME].load(Ordering::Relaxed)
        };
        let width_ms = self.shared.params[P_WIDTH].load(Ordering::Relaxed);
        let target_l = time_ms * 0.001 * sr;
        let target_r = (time_ms + width_ms).max(1.0) * 0.001 * sr;

        let mode = self.shared.params[P_MODE]
            .load(Ordering::Relaxed)
            .round() as u32;
        let feedback_target = self.shared.params[P_FEEDBACK].load(Ordering::Relaxed);
        let tone_target = self.shared.params[P_TONE].load(Ordering::Relaxed);
        let drive_target = self.shared.params[P_DRIVE].load(Ordering::Relaxed);
        let mix_target = self.shared.params[P_MIX].load(Ordering::Relaxed);
        let duck_amount_target = self.shared.params[P_DUCK_AMOUNT].load(Ordering::Relaxed);
        let duck_attack = self.shared.params[P_DUCK_ATTACK].load(Ordering::Relaxed);
        let duck_release = self.shared.params[P_DUCK_RELEASE].load(Ordering::Relaxed);

        // ---- Snapshot the sidechain (port 1) ----
        let frames = audio.frames_count() as usize;
        let n_frames = frames.min(self.sc_l.len());
        let mut sc_present = false;
        if let Some(sc_port) = audio.input_port(1) {
            if let Some(chans) = sc_port.channels()?.into_f32() {
                if let Some(l) = chans.channel(0) {
                    let n = n_frames.min(l.len());
                    self.sc_l[..n].copy_from_slice(&l[..n]);
                    if l.iter().take(n).any(|&x| x != 0.0) { sc_present = true; }
                }
                if let Some(r) = chans.channel(1) {
                    let n = n_frames.min(r.len());
                    self.sc_r[..n].copy_from_slice(&r[..n]);
                    if r.iter().take(n).any(|&x| x != 0.0) { sc_present = true; }
                } else {
                    self.sc_r[..n_frames].copy_from_slice(&self.sc_l[..n_frames]);
                }
            }
        }

        // ---- Process main port ----
        if let Some(mut main_pair) = audio.port_pair(0) {
            let Some(channel_pairs) = main_pair.channels()?.into_f32() else {
                return Ok(ProcessStatus::Continue);
            };
            let mut iter = channel_pairs.into_iter();
            let Some(ch_l) = iter.next() else { return Ok(ProcessStatus::Continue); };
            let ch_r = iter.next();
            stereo_process(
                self,
                ch_l, ch_r, sr, bypassed, mode,
                target_l, target_r,
                feedback_target, tone_target, drive_target, mix_target,
                sc_present, duck_amount_target, duck_attack, duck_release,
            );
        }

        Ok(ProcessStatus::Continue)
    }
}

#[allow(clippy::too_many_arguments)]
fn stereo_process(
    p: &mut PluginAudioProcessor<'_>,
    ch_l: ChannelPair<'_, f32>,
    ch_r: Option<ChannelPair<'_, f32>>,
    sr: f32,
    bypassed: bool,
    mode: u32,
    target_l: f32,
    target_r: f32,
    feedback_target: f32,
    tone_target: f32,
    drive_target: f32,
    mix_target: f32,
    sc_present: bool,
    duck_amount_target: f32,
    duck_attack: f32,
    duck_release: f32,
) {
    use superduper_dsp_sdk::clap_helpers::split_io;
    let Some((l_read, l_write)) = split_io(ch_l) else { return };
    let r = ch_r.and_then(split_io);

    if bypassed {
        l_write.copy_from_slice(l_read);
        if let Some((rr, rw)) = r { rw.copy_from_slice(rr); }
        return;
    }

    // Slap mode: short fixed delay (~25 ms) on one side, no feedback.
    let slap_mode = mode == 2;
    let pingpong_mode = mode == 1;

    let Some((r_read, r_write)) = r else {
        // Mono fallback.
        let n = l_read.len();
        for i in 0..n {
            let dry = l_read[i];
            let mix = p.smooth_mix.step(mix_target, sr);
            let feedback = p.smooth_feedback.step(feedback_target, sr);
            let tone = p.smooth_tone.step(tone_target, sr);
            let drive = p.smooth_drive.step(drive_target, sr);
            let duck_amount = p.smooth_duck.step(duck_amount_target, sr);
            let drive_lin = 10f32.powf(drive / 20.0);

            // Ducking key: external sidechain if routed, otherwise dry.
            let key = if sc_present {
                p.sc_l.get(i).copied().unwrap_or(0.0)
            } else { dry };
            let duck_gain = p.ducker.process(key, key, sr, duck_amount, duck_attack, duck_release);

            let time_l = p.slew_time_l.step(target_l, sr, 30.0);
            let cleaned = p.dc_l.process(dry);
            let tap_l = p.delay_l.read_lagrange3(time_l);
            let filtered = p.tone_l.process(tap_l, sr, tone);
            let saturated = tape_clip(filtered, drive_lin);
            let fb_signal = if slap_mode { 0.0 } else { saturated * feedback };
            p.delay_l.write(cleaned + fb_signal);
            p.fb_l = saturated;

            // Duck applies to WET only — dry passes through clean.
            let final_out = dry * (1.0 - mix) + saturated * duck_gain * mix;
            l_write[i] = final_out;
            p.shared.scope.push(final_out);
        }
        return;
    };

    let n = l_read.len().min(r_read.len());
    for i in 0..n {
        let dry_l = l_read[i];
        let dry_r = r_read[i];

        // Per-sample smoothing of every user-facing knob.
        let mix = p.smooth_mix.step(mix_target, sr);
        let feedback = p.smooth_feedback.step(feedback_target, sr);
        let tone = p.smooth_tone.step(tone_target, sr);
        let drive = p.smooth_drive.step(drive_target, sr);
        let duck_amount = p.smooth_duck.step(duck_amount_target, sr);
        let drive_lin = 10f32.powf(drive / 20.0);

        // Ducking key — sidechain if routed, else dry stereo.
        let (key_l, key_r) = if sc_present {
            (
                p.sc_l.get(i).copied().unwrap_or(0.0),
                p.sc_r.get(i).copied().unwrap_or(0.0),
            )
        } else {
            (dry_l, dry_r)
        };
        let duck_gain = p.ducker.process(
            key_l, key_r, sr, duck_amount, duck_attack, duck_release,
        );

        // Time slew — gives tape pitch sweep on knob movement.
        let time_l = p.slew_time_l.step(target_l, sr, 30.0);
        // In slap mode, force a short fixed time on R (Haas).
        let r_target = if slap_mode { 0.025 * sr } else { target_r };
        let time_r = p.slew_time_r.step(r_target, sr, 30.0);

        // DC-block dry input before it enters the feedback loop.
        let in_l = p.dc_l.process(dry_l);
        let in_r = p.dc_r.process(dry_r);

        // Read taps first.
        let tap_l = p.delay_l.read_lagrange3(time_l);
        let tap_r = p.delay_r.read_lagrange3(time_r);

        // In-loop tone + saturation (per channel). This is where the
        // "every repeat degrades" character lives — the LP cuts highs
        // each iteration, tape_clip rounds the peaks.
        let toned_l = p.tone_l.process(tap_l, sr, tone);
        let toned_r = p.tone_r.process(tap_r, sr, tone);
        let sat_l = tape_clip(toned_l, drive_lin);
        let sat_r = tape_clip(toned_r, drive_lin);

        // Determine what feeds each delay line's input next sample.
        let (write_l_input, write_r_input);
        match (pingpong_mode, slap_mode) {
            (true, _) => {
                // Ping-pong: input goes into L only (summed). L's tap feeds R.
                // R's tap feeds L. Cross-feedback = signature L-R-L-R bouncing.
                let mono_in = (in_l + in_r) * 0.5;
                write_l_input = mono_in + sat_r * feedback;
                write_r_input = sat_l * feedback;
            }
            (_, true) => {
                // Slap (Haas): no feedback at all, L straight in, R = delayed L.
                write_l_input = in_l;
                write_r_input = in_l; // R delay reads its own buffer
            }
            _ => {
                // Stereo: each channel keeps its own loop, with a tiny dose of
                // cross-feedback so big feedback values still feel like one
                // unified reverb-ish wash. 80% own + 20% cross — classic feel.
                write_l_input = in_l + (sat_l * 0.8 + sat_r * 0.2) * feedback;
                write_r_input = in_r + (sat_r * 0.8 + sat_l * 0.2) * feedback;
            }
        }

        p.delay_l.write(write_l_input);
        p.delay_r.write(write_r_input);
        p.fb_l = sat_l;
        p.fb_r = sat_r;

        // Final mix — dry passes through unaffected; wet is ducked by the
        // sidechain envelope so a loud vocal pushes the delay tail back.
        let out_l = dry_l * (1.0 - mix) + sat_l * duck_gain * mix;
        let out_r = dry_r * (1.0 - mix) + sat_r * duck_gain * mix;
        l_write[i] = out_l;
        r_write[i] = out_r;
        p.shared.scope.push(0.5 * (out_l + out_r));
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions (same shape as the other effects)
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input { 2 } else { 1 }
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
                name: b"Sidechain",
                channel_count: 2,
                flags: AudioPortFlags::empty(),
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            }),
            _ => {}
        }
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
        if id.get() as usize == P_TIME_DIV {
            return write!(w, "{}", superduper_synth_core::dsp_blocks::sync_division_label(v.round() as u32));
        }
        if id.get() as usize == P_TIME_SYNC {
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

pub struct SuperDuperDelay;

impl Plugin for SuperDuperDelay {
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

impl DefaultPluginFactory for SuperDuperDelay {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.delay",
            plugin_display_name!("SuperDuper Delay"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Stereo delay with Lagrange interp and tape-style feedback")
        .with_features([AUDIO_EFFECT, STEREO, DELAY])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Delay — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperDelay>);
