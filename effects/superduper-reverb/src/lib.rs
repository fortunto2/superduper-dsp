//! SuperDuper Reverb — Dattorro plate reverb as a standalone CLAP plugin.
//!
//! See `plate.rs` for the full DSP. In short: Dattorro's 1997 figure-of-eight
//! plate (the same topology behind Lexicon 224 and Valhalla VintageVerb) —
//! input diffuser → two crossfeeding tanks with modulated allpasses and
//! in-loop damping → multi-tap output. Real stereo width, smooth tail.

#![allow(clippy::missing_safety_doc)]

pub mod plate;
pub mod gui;
pub mod presets;
pub use plate::{PlateParams, PlateState};

use atomic_float::AtomicF32;

// ---------------------------------------------------------------------------
// Debug logging — same pattern as the main plugin's `dlog!`. Writes to
// `~/.superduper-dsp/reverb.log` so a Dock-launched DAW doesn't swallow it.
// Strictly for development; in a shipping build we'd gate this behind a
// feature flag.
// ---------------------------------------------------------------------------

fn init_logging() { superduper_dsp_sdk::log::init("reverb"); }
use superduper_dsp_sdk::slog;
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_common::stream::{InputStream, OutputStream};
use clack_extensions::state::{PluginState, PluginStateImpl};

use clack_plugin::prelude::*;
use clack_plugin::plugin::features::*;
use std::ffi::CStr;
use std::sync::atomic::Ordering;

// ===========================================================================
// Parameter table — all fixed, no runtime changes (this is what makes
// REAPER's UI cache work correctly).
// ===========================================================================

use superduper_dsp_sdk::clap_helpers::ParamDef;

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Size",          min: 0.1, max: 1.5, default: 0.7,  unit: ""   },
    ParamDef { id: 1, name: b"Decay",         min: 0.0, max: 0.95, default: 0.7, unit: ""   },
    ParamDef { id: 2, name: b"Damping",       min: 0.0, max: 1.0,  default: 0.4, unit: ""   },
    ParamDef { id: 3, name: b"Pre-Delay",     min: 0.0, max: 200.0, default: 10.0, unit: "ms" },
    ParamDef { id: 4, name: b"Modulation",    min: 0.0, max: 1.0,  default: 0.3, unit: ""   },
    ParamDef { id: 5, name: b"Width",         min: 0.0, max: 1.0,  default: 1.0, unit: ""   },
    ParamDef { id: 6, name: b"Mix",           min: 0.0, max: 1.0,  default: 0.3, unit: ""   },
    // Ducking — key signal is sidechain port if connected, otherwise dry input.
    ParamDef { id: 7, name: b"Duck Amount",   min: 0.0, max: 24.0, default: 0.0, unit: "dB" },
    ParamDef { id: 8, name: b"Duck Attack",   min: 1.0, max: 200.0, default: 5.0, unit: "ms" },
    ParamDef { id: 9, name: b"Duck Release",  min: 10.0, max: 1000.0, default: 150.0, unit: "ms" },
    // Pre-delay tempo sync — if on, Pre-Delay is computed from host BPM × PD Div.
    ParamDef { id: 10, name: b"PD Sync",      min: 0.0, max: 1.0,  default: 0.0, unit: "" },
    ParamDef { id: 11, name: b"PD Div",       min: 0.0, max: 11.0, default: 10.0, unit: "" },
    // Freeze toggle — when on, decay → ∞ and input is muted into the tank.
    // The existing tail keeps recirculating with no damping loss.
    ParamDef { id: 12, name: b"Freeze",       min: 0.0, max: 1.0,  default: 0.0, unit: "" },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[12];

pub const P_SIZE: usize = 0;
pub const P_DECAY: usize = 1;
pub const P_DAMP: usize = 2;
pub const P_PREDELAY: usize = 3;
pub const P_MOD: usize = 4;
pub const P_WIDTH: usize = 5;
pub const P_MIX: usize = 6;
pub const P_DUCK_AMOUNT: usize = 7;
pub const P_DUCK_ATTACK: usize = 8;
pub const P_DUCK_RELEASE: usize = 9;
pub const P_PD_SYNC: usize = 10;
pub const P_PD_DIV: usize = 11;
pub const P_FREEZE: usize = 12;


// ===========================================================================
// CLAP wiring
// ===========================================================================

/// Atomics live in an Arc so the GUI thread (egui_baseview) can hold its own
/// clone and read/write them without lifetime tangles with the host's
/// PluginShared ownership.
pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: std::sync::atomic::AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    /// Per-param dirty flag — GUI sets it on user-driven change; the
    /// audio thread emits a `ParamValueEvent` to the host on the next
    /// process() so REAPER can record the move into the automation lane.
    pub dirty_params: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_begin: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_end: [std::sync::atomic::AtomicBool; PARAMS.len()],
    /// Currently-selected preset index — persisted via simple_state
    /// so the dropdown survives project reopens.
    pub active_preset: std::sync::atomic::AtomicU32,
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
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
                host_bpm: AtomicF32::new(120.0),
            }),
        }
    }
    /// Clone the Arc for handing to the GUI thread.
    pub fn shared_handle(&self) -> SharedParams { std::sync::Arc::clone(&self.inner) }
}

// Auto-deref so existing call sites `shared.params[i]` and `shared.bypass.load(...)`
// keep compiling without per-line rewrites.
impl std::ops::Deref for PluginShared {
    type Target = SharedParamsInner;
    fn deref(&self) -> &SharedParamsInner { &self.inner }
}

impl Default for PluginShared {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    /// Open egui-baseview window. Created in `set_parent`, dropped in
    /// `destroy()` so the window goes away cleanly when the host closes the
    /// FX editor.
    gui_handle: Option<baseview::WindowHandle>,
    /// Shared with the egui update closure so we can push host-driven resize
    /// requests across the thread boundary.
    gui_resize: gui::ResizeBridge,
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    state: Box<PlateState>,
    ducker: plate::Ducker,
    // Per-sample smoothed versions of the most user-visible knobs.
    // Without this, dragging Mix or Width sends a step function into the
    // audio path and produces audible zipper noise.
    smooth_mix: SmoothedParam,
    smooth_width: SmoothedParam,
    smooth_duck: SmoothedParam,
    // SIZE governs every tank delay length AND the 7-tap output offsets.
    // A step-jump in size (knob drag) used to click loudly because the
    // tank delays read by integer offset. Now that taps are fractional
    // (plate::Delay::tap / Allpass::tap_buf), a smoothed size sweep is
    // continuous in audio output. ~30 ms TC is short enough to feel
    // immediate but long enough to glide through several integer-sample
    // boundaries before the next user knob update.
    smooth_size: SmoothedParam,
    smooth_decay: SmoothedParam,
    smooth_damp: SmoothedParam,
    smooth_predelay: SmoothedParam,
    smooth_mod: SmoothedParam,
    // Scratch buffers for sidechain (port index 1). Pre-allocated at
    // `activate` so the audio thread never touches the allocator.
    sc_l: Box<[f32]>,
    sc_r: Box<[f32]>,
    sample_rate: f32,
}

/// Walk left & right channel buffers in lockstep, feeding each (L,R) sample
/// pair into the stereo Dattorro tank and writing the wet/dry-mixed result
/// back. Handles every CLAP buffer mode (InputOutput / InPlace / etc.).
///
/// Ducking: the key signal is either an external sidechain (when `sc` is
/// `Some`, host actively routes audio to port 1) or the dry input itself
/// (the "auto" mode that works on plain insert vocals). The wet portion of
/// the output is attenuated by the envelope-derived gain.
#[allow(clippy::too_many_arguments)]
fn stereo_process(
    state: &mut PlateState,
    ducker: &mut plate::Ducker,
    smooth_mix: &mut SmoothedParam,
    smooth_width: &mut SmoothedParam,
    smooth_duck: &mut SmoothedParam,
    smooth_size: &mut SmoothedParam,
    smooth_decay: &mut SmoothedParam,
    smooth_damp: &mut SmoothedParam,
    smooth_predelay: &mut SmoothedParam,
    smooth_mod: &mut SmoothedParam,
    ch_l: ChannelPair<'_, f32>,
    ch_r: Option<ChannelPair<'_, f32>>,
    sc: Option<(&[f32], &[f32])>,
    sr: f32,
    size_target: f32,
    decay_target: f32,
    damp_target: f32,
    predelay_target: f32,
    mod_target: f32,
    width_target: f32,
    mix_target: f32,
    duck_amount_target: f32,
    duck_attack_ms: f32,
    duck_release_ms: f32,
    bypassed: bool,
    freeze: bool,
    scope: &superduper_synth_core::gui::LiveScope,
) {
    // Resolve each channel into (read_slice, write_slice). For InPlace we
    // reuse the same buffer for both. OutputOnly clears, InputOnly is a no-op.
    fn split<'b>(c: ChannelPair<'b, f32>) -> Option<(&'b [f32], &'b mut [f32])> {
        match c {
            ChannelPair::InputOutput(i, o) => Some((i, o)),
            ChannelPair::InPlace(buf) => {
                // SAFETY: aliasing read+write is fine here because we only read
                // a sample before overwriting that same index.
                let ptr = buf.as_mut_ptr();
                let len = buf.len();
                let read = unsafe { core::slice::from_raw_parts(ptr, len) };
                let write = unsafe { core::slice::from_raw_parts_mut(ptr, len) };
                Some((read, write))
            }
            ChannelPair::OutputOnly(buf) => {
                buf.fill(0.0);
                None
            }
            ChannelPair::InputOnly(_) => None,
        }
    }

    let l = split(ch_l);
    let r = ch_r.and_then(split);

    let (l_read, l_write) = match l {
        Some(v) => v,
        None => return,
    };

    // Mono case: just feed (x, x) through the tank and write back the L tap.
    let Some((r_read, r_write)) = r else {
        if bypassed {
            l_write.copy_from_slice(l_read);
            return;
        }
        for (i, (inp, o)) in l_read.iter().zip(l_write.iter_mut()).enumerate() {
            let mix = smooth_mix.step(mix_target, sr);
            let width = smooth_width.step(width_target, sr);
            let duck_amount_db = smooth_duck.step(duck_amount_target, sr);
            let size_now = smooth_size.step(size_target, sr);
            let decay_now = smooth_decay.step(decay_target, sr);
            let damp_now = smooth_damp.step(damp_target, sr);
            let predelay_now = smooth_predelay.step(predelay_target, sr);
            let mod_now = smooth_mod.step(mod_target, sr);
            let p = PlateParams {
                sr,
                size: size_now,
                decay: decay_now,
                damp: damp_now,
                bandwidth: 1.0 - damp_now * 0.5,
                predelay_ms: predelay_now,
                modulation: mod_now,
            };

            let dry = *inp;
            let key_l = sc.map(|(s, _)| s.get(i).copied().unwrap_or(0.0)).unwrap_or(dry);
            let duck_gain = ducker.process(
                key_l, key_l, sr,
                duck_amount_db, duck_attack_ms, duck_release_ms,
            );
            // Freeze: mute dry → tank so the existing tail recirculates
            // without being overwritten by new input. The dry-blend in
            // the output mix still uses the live dry sample so the user
            // hears what they're playing on top of the held tail.
            let tank_in = if freeze { 0.0 } else { dry };
            let (wl, _) = state.process_sample(tank_in, tank_in, p);
            let final_out = dry * (1.0 - mix) + (wl * width * duck_gain) * mix;
            *o = final_out;
            scope.push(final_out);
        }
        return;
    };

    if bypassed {
        l_write.copy_from_slice(l_read);
        r_write.copy_from_slice(r_read);
        return;
    }

    let n = l_read.len().min(r_read.len());
    for i in 0..n {
        // Slew the user-facing knobs toward their targets, sample by sample.
        let mix = smooth_mix.step(mix_target, sr);
        let width = smooth_width.step(width_target, sr);
        let duck_amount_db = smooth_duck.step(duck_amount_target, sr);
        let size_now = smooth_size.step(size_target, sr);
        let decay_now = smooth_decay.step(decay_target, sr);
        let damp_now = smooth_damp.step(damp_target, sr);
        let predelay_now = smooth_predelay.step(predelay_target, sr);
        let mod_now = smooth_mod.step(mod_target, sr);
        let p = PlateParams {
            sr,
            size: size_now,
            decay: decay_now,
            damp: damp_now,
            bandwidth: 1.0 - damp_now * 0.5,
            predelay_ms: predelay_now,
            modulation: mod_now,
        };

        let dl = l_read[i];
        let dr = r_read[i];

        // Key signal selection. Shadow `sr` from the outer scope inside the
        // pattern binding (the sidechain right channel is also named `sr` —
        // rename to `scr` so we don't accidentally use the audio rate).
        let (key_l, key_r) = match sc {
            Some((sl, scr)) => (sl.get(i).copied().unwrap_or(0.0), scr.get(i).copied().unwrap_or(0.0)),
            None => (dl, dr),
        };
        let duck_gain = ducker.process(
            key_l, key_r, sr,
            duck_amount_db, duck_attack_ms, duck_release_ms,
        );

        let (tank_in_l, tank_in_r) = if freeze { (0.0, 0.0) } else { (dl, dr) };
        let (wl, wr) = state.process_sample(tank_in_l, tank_in_r, p);
        let mono_w = (wl + wr) * 0.5;
        let final_wl = wl * width + mono_w * (1.0 - width);
        let final_wr = wr * width + mono_w * (1.0 - width);
        let out_l_sample = dl * (1.0 - mix) + final_wl * duck_gain * mix;
        let out_r_sample = dr * (1.0 - mix) + final_wr * duck_gain * mix;
        l_write[i] = out_l_sample;
        r_write[i] = out_r_sample;
        scope.push((out_l_sample + out_r_sample) * 0.5);
    }
}

/// Thin wrapper around the shared CLAP helper that takes our typed PluginShared.
fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
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
        slog!(
            "activate: sr={}, frames={}..={}",
            audio_config.sample_rate, audio_config.min_frames_count, audio_config.max_frames_count
        );
        let max_frames = audio_config.max_frames_count as usize;
        // Snap smoothers to the current host-loaded param values so the
        // first block doesn't slew in from 0.
        let init_mix = shared.params[P_MIX].load(Ordering::Relaxed);
        let init_width = shared.params[P_WIDTH].load(Ordering::Relaxed);
        let init_duck = shared.params[P_DUCK_AMOUNT].load(Ordering::Relaxed);
        let init_size = shared.params[P_SIZE].load(Ordering::Relaxed);
        let init_decay = shared.params[P_DECAY].load(Ordering::Relaxed);
        let init_damp = shared.params[P_DAMP].load(Ordering::Relaxed);
        let init_predelay = shared.params[P_PREDELAY].load(Ordering::Relaxed);
        let init_mod = shared.params[P_MOD].load(Ordering::Relaxed);
        Ok(Self {
            shared,
            state: Box::new(PlateState::default()),
            ducker: plate::Ducker::default(),
            smooth_mix: SmoothedParam::new(init_mix),
            smooth_width: SmoothedParam::new(init_width),
            smooth_duck: SmoothedParam::new(init_duck),
            smooth_size: SmoothedParam::new(init_size),
            smooth_decay: SmoothedParam::new(init_decay),
            smooth_damp: SmoothedParam::new(init_damp),
            smooth_predelay: SmoothedParam::new(init_predelay),
            smooth_mod: SmoothedParam::new(init_mod),
            sc_l: vec![0.0; max_frames].into_boxed_slice(),
            sc_r: vec![0.0; max_frames].into_boxed_slice(),
            sample_rate: audio_config.sample_rate as f32,
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
        // Push every GUI-driven param change into the host output queue
        // so REAPER can record the move into the automation lane.
        for (i, flag) in self.shared.dirty_params.iter().enumerate() {
            if flag.swap(false, Ordering::AcqRel) {
                let value = self.shared.params[i].load(Ordering::Relaxed) as f64;
                let ev = clack_common::events::event_types::ParamValueEvent::new(
                    0,
                    ClapId::new(i as u32),
                    clack_common::events::Pckn::new(0u16, 0u16, 0u16, 0u32),
                    value,
                    clack_common::utils::Cookie::empty(),
                );
                let _ = events.output.try_push(&ev);
            }
        }
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(
            &self.shared.gesture_begin,
            &self.shared.gesture_end,
            events.output,
        );

        let size_target = self.shared.params[P_SIZE].load(Ordering::Relaxed);
        let freeze_on = self.shared.params[P_FREEZE].load(Ordering::Relaxed) >= 0.5;
        // Freeze raises decay to its cap (effectively infinite tail) and
        // mutes input into the tank below. We don't disable damping — a
        // pure-reflective infinite tail goes harsh fast; some HF rolloff
        // keeps the freeze musical (Lexicon-style Hold mode).
        let raw_decay = self.shared.params[P_DECAY].load(Ordering::Relaxed);
        let decay_target = if freeze_on { 0.99 } else { raw_decay };
        let damp_target = self.shared.params[P_DAMP].load(Ordering::Relaxed);
        // Pre-delay tempo sync: if PD Sync is on, pre-delay = note ms
        // derived from host BPM × PD Div (same musical-division enum
        // shape as Delay's Time Div).
        let pd_sync_on = self.shared.params[P_PD_SYNC].load(Ordering::Relaxed) >= 0.5;
        let predelay_target = if pd_sync_on {
            let div = self.shared.params[P_PD_DIV].load(Ordering::Relaxed) as u32;
            let bpm = self.shared.host_bpm.load(Ordering::Relaxed);
            let hz = superduper_synth_core::dsp_blocks::sync_division_hz(div, bpm);
            (1000.0 / hz.max(0.5)).clamp(0.0, 200.0)
        } else {
            self.shared.params[P_PREDELAY].load(Ordering::Relaxed)
        };
        let mod_target = self.shared.params[P_MOD].load(Ordering::Relaxed);
        let width = self.shared.params[P_WIDTH].load(Ordering::Relaxed);
        let mix = self.shared.params[P_MIX].load(Ordering::Relaxed);
        let duck_amount = self.shared.params[P_DUCK_AMOUNT].load(Ordering::Relaxed);
        let duck_attack = self.shared.params[P_DUCK_ATTACK].load(Ordering::Relaxed);
        let duck_release = self.shared.params[P_DUCK_RELEASE].load(Ordering::Relaxed);
        let bypassed = self.shared.bypass.load(Ordering::Relaxed);

        // ---- Step 1: snapshot sidechain (port 1) into our scratch buffers.
        // If the host left it unrouted, both channels will be all-zero and
        // we'll fall back to dry input as the key signal further down.
        let frames = audio.frames_count() as usize;
        let n_frames = frames.min(self.sc_l.len());
        let mut sc_present = false;
        if let Some(sc_port) = audio.input_port(1) {
            if let Some(chans) = sc_port.channels()?.into_f32() {
                if let Some(l) = chans.channel(0) {
                    let n = n_frames.min(l.len());
                    self.sc_l[..n].copy_from_slice(&l[..n]);
                    if l.iter().take(n).any(|&x| x != 0.0) {
                        sc_present = true;
                    }
                }
                if let Some(r) = chans.channel(1) {
                    let n = n_frames.min(r.len());
                    self.sc_r[..n].copy_from_slice(&r[..n]);
                    if r.iter().take(n).any(|&x| x != 0.0) {
                        sc_present = true;
                    }
                } else {
                    // Mono sidechain → mirror L into R.
                    self.sc_r[..n_frames].copy_from_slice(&self.sc_l[..n_frames]);
                }
            }
        }

        // ---- Step 2: process main port (index 0). Key signal is sidechain
        // if it carries audio, otherwise the dry input itself.
        if let Some(mut main_pair) = audio.port_pair(0) {
            let Some(channel_pairs) = main_pair.channels()?.into_f32() else {
                return Ok(ProcessStatus::Continue);
            };
            let mut iter = channel_pairs.into_iter();
            let Some(ch_l) = iter.next() else {
                return Ok(ProcessStatus::Continue);
            };
            let ch_r = iter.next();
            let sc_slice = if sc_present {
                Some((&self.sc_l[..n_frames], &self.sc_r[..n_frames]))
            } else {
                None
            };
            stereo_process(
                self.state.as_mut(),
                &mut self.ducker,
                &mut self.smooth_mix,
                &mut self.smooth_width,
                &mut self.smooth_duck,
                &mut self.smooth_size,
                &mut self.smooth_decay,
                &mut self.smooth_damp,
                &mut self.smooth_predelay,
                &mut self.smooth_mod,
                ch_l,
                ch_r,
                sc_slice,
                self.sample_rate,
                size_target,
                decay_target,
                damp_target,
                predelay_target,
                mod_target,
                width,
                mix,
                duck_amount,
                duck_attack,
                duck_release,
                bypassed,
                freeze_on,
                &self.shared.scope,
            );
        }

        Ok(ProcessStatus::Continue)
    }
}

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input { 2 } else { 1 } // main I/O + sidechain input
    }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        match (index, is_input) {
            (0, _) => {
                writer.set(&AudioPortInfo {
                    id: ClapId::new(0),
                    name: if is_input { b"Input" } else { b"Output" },
                    channel_count: 2,
                    flags: AudioPortFlags::IS_MAIN,
                    port_type: Some(AudioPortType::STEREO),
                    // No in-place: the host must hand us separate input and output
                // buffers. With in-place allowed, split_io had to return a
                // &[f32] and a &mut [f32] over the SAME memory to keep the
                // "read x[i], write y[i]" style working — two noalias slices
                // aliasing each other, which is undefined behaviour whatever
                // the access order. The host's own copy costs the same as the
                // scratch buffer we would otherwise keep per plugin.
                in_place_pair: None,
                });
            }
            (1, true) => {
                // Sidechain — IS_MAIN is OFF. Hosts may leave this unrouted;
                // we fall back to dry input as the key signal when so.
                writer.set(&AudioPortInfo {
                    id: ClapId::new(1),
                    name: b"Sidechain",
                    channel_count: 2,
                    flags: AudioPortFlags::empty(),
                    port_type: Some(AudioPortType::STEREO),
                    in_place_pair: None,
                });
            }
            _ => {}
        }
    }
}

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 { PARAMS.len() as u32 }

    fn get_info(&mut self, param_index: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info_stepped(PARAMS, param_index, info, STEPPED_PARAMS);
    }

    fn get_value(&mut self, param_id: ClapId) -> Option<f64> {
        let i = param_id.get() as usize;
        self.shared.params.get(i).map(|a| a.load(Ordering::Relaxed) as f64)
    }

    fn value_to_text(
        &mut self,
        param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        use core::fmt::Write;
        let pid = param_id.get() as usize;
        if pid == P_PD_DIV {
            return write!(writer, "{}",
                superduper_synth_core::dsp_blocks::sync_division_label(value.round() as u32));
        }
        if pid == P_PD_SYNC || pid == P_FREEZE {
            return write!(writer, "{}", if value >= 0.5 { "On" } else { "Off" });
        }
        ParamDef::write_display(PARAMS, param_id, value, writer)
    }

    fn text_to_value(&mut self, param_id: ClapId, text: &CStr) -> Option<f64> {
        ParamDef::parse_text(PARAMS, param_id, text)
    }

    fn flush(&mut self, input_events: &InputEvents, _output_events: &mut OutputEvents) {
        apply_param_events(self.shared, input_events);
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, input_events: &InputEvents, _output_events: &mut OutputEvents) {
        apply_param_events(self.shared, input_events);
    }
}

// ---------------------------------------------------------------------------
// CLAP state — params + bypass through the shared SDK helper. Without this
// REAPER drops everything when saving the project / FX chain preset.
// ---------------------------------------------------------------------------

superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);


// ===========================================================================
// CLAP GUI extension — embedded egui_baseview window.
// ===========================================================================

use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window as ClapGuiWindow,
};
use std::sync::atomic::Ordering as AtomicOrdering;

impl PluginGuiImpl for PluginMainThread<'_> {
    fn is_api_supported(&mut self, config: GuiConfiguration) -> bool {
        if config.is_floating {
            return false;
        }
        config.api_type == GuiApiType::COCOA
            || config.api_type == GuiApiType::WIN32
            || config.api_type == GuiApiType::X11
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

    fn create(&mut self, _config: GuiConfiguration) -> Result<(), PluginError> {
        slog!("gui::create");
        Ok(())
    }

    fn destroy(&mut self) {
        slog!("gui::destroy");
        self.gui_handle = None;
    }

    fn set_scale(&mut self, _scale: f64) -> Result<(), PluginError> { Ok(()) }

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

    fn adjust_size(&mut self, size: GuiSize) -> Option<GuiSize> {
        Some(GuiSize {
            width: size.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH),
            height: size.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT),
        })
    }

    fn set_size(&mut self, size: GuiSize) -> Result<(), PluginError> {
        let w = size.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH);
        let h = size.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT);
        self.gui_resize.0.store(w, AtomicOrdering::Relaxed);
        self.gui_resize.1.store(h, AtomicOrdering::Relaxed);
        Ok(())
    }

    fn set_parent(&mut self, window: ClapGuiWindow) -> Result<(), PluginError> {
        slog!("gui::set_parent (api={:?})", window.api_type());
        let shared = self.shared.shared_handle();
        let handle = gui::open_window(&window, shared, self.gui_resize.clone());
        self.gui_handle = Some(handle);
        Ok(())
    }

    fn set_transient(&mut self, _window: ClapGuiWindow) -> Result<(), PluginError> { Ok(()) }
    fn show(&mut self) -> Result<(), PluginError> { slog!("gui::show"); Ok(()) }
    fn hide(&mut self) -> Result<(), PluginError> { slog!("gui::hide"); Ok(()) }
}

pub struct SuperDuperReverb;

impl Plugin for SuperDuperReverb {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(
        builder: &mut PluginExtensions<Self>,
        _shared: Option<&Self::Shared<'_>>,
    ) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>();
    }
}

// Build identity comes from the shared sdk-build helper — every plugin in
// this workspace gets the same `[b NNNNN]` suffix + version string format.
// CLAP id stays stable so REAPER's track-level caches are preserved.
use superduper_dsp_sdk::{build_num, build_date, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::SmoothedParam;

impl DefaultPluginFactory for SuperDuperReverb {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.reverb",
            plugin_display_name!("SuperDuper Reverb"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.2"))
        .with_description("Stereo Dattorro plate reverb")
        .with_features([AUDIO_EFFECT, STEREO, REVERB])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!(
            "new_shared: SuperDuper Reverb — build {} ({})",
            build_num!(),
            build_date!()
        );
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

clack_export_entry!(SinglePluginEntry<SuperDuperReverb>);
