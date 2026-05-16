//! SuperDuper Reverb — stereo plate reverb as a standalone CLAP plugin.
//!
//! Why this exists separately: the main SuperDuper DSP plugin hot-loads
//! arbitrary effect dylibs, which means its CLAP param layout is dynamic
//! and REAPER's generic UI caching makes the slider labels go out of sync.
//! For shipping a real-world effect to other people, a fixed-layout
//! standalone plugin sidesteps all of that.
//!
//! Architecture: Schroeder reverb (Manfred Schroeder, 1962), still the
//! starting point for almost every "plate" / "hall" reverb worth shipping.
//!
//!   pre-delay → [4 lowpass-feedback combs in parallel] → [4 allpasses in series]
//!
//! - Combs with mutually-prime delay lengths give a dense early-reflection
//!   tail without resonant peaks.
//! - Allpasses smear the comb teeth into a smooth tail.
//! - One-pole lowpass inside each comb's feedback path emulates plate
//!   high-frequency loss.
//! - LFO-modulated read positions inside the combs give the slow chorus
//!   shimmer of a real plate.

#![allow(clippy::missing_safety_doc)]

use atomic_float::AtomicF32;
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_plugin::events::event_types::ParamValueEvent;
use clack_plugin::prelude::*;
use clack_plugin::plugin::features::*;
use std::ffi::CStr;
use std::sync::atomic::Ordering;

// ===========================================================================
// Parameter table — all fixed, no runtime changes (this is what makes
// REAPER's UI cache work correctly).
// ===========================================================================

#[derive(Copy, Clone)]
struct ParamDef {
    id: u32,
    name: &'static [u8],
    min: f64,
    max: f64,
    default: f64,
    unit: &'static str,
}

const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Size",       min: 0.1, max: 1.5, default: 0.7,  unit: ""    },
    ParamDef { id: 1, name: b"Decay",      min: 0.0, max: 0.95, default: 0.7, unit: ""    },
    ParamDef { id: 2, name: b"Damping",    min: 0.0, max: 1.0,  default: 0.4, unit: ""    },
    ParamDef { id: 3, name: b"Pre-Delay",  min: 0.0, max: 200.0, default: 10.0, unit: "ms" },
    ParamDef { id: 4, name: b"Modulation", min: 0.0, max: 1.0,  default: 0.3, unit: ""    },
    ParamDef { id: 5, name: b"Width",      min: 0.0, max: 1.0,  default: 1.0, unit: ""    },
    ParamDef { id: 6, name: b"Mix",        min: 0.0, max: 1.0,  default: 0.3, unit: ""    },
];

const P_SIZE: usize = 0;
const P_DECAY: usize = 1;
const P_DAMP: usize = 2;
const P_PREDELAY: usize = 3;
const P_MOD: usize = 4;
const P_WIDTH: usize = 5;
const P_MIX: usize = 6;

fn pdef(id: u32) -> Option<&'static ParamDef> {
    PARAMS.iter().find(|p| p.id == id)
}

// ===========================================================================
// Reverb engine — Schroeder, per-channel state for true stereo.
// ===========================================================================

// Mutually-prime sample counts (chosen near a 44.1 kHz reference). At higher
// sample rates the Size knob scales these, so we use the same numbers and
// scale at runtime.
const COMB_LENS:    [usize; 4] = [1116, 1188, 1277, 1356];
const ALLPASS_LENS: [usize; 4] = [556, 441, 341, 225];
const PREDELAY_MAX_SAMPLES: usize = 96000;     // 1 sec at 96 kHz, 2 sec at 48 kHz
const COMB_BUF_CAP:    usize = 4096;            // headroom for Size > 1.0
const ALLPASS_BUF_CAP: usize = 1024;

struct Comb {
    buf: [f32; COMB_BUF_CAP],
    idx: usize,
    lp: f32,
}

impl Default for Comb {
    fn default() -> Self {
        Self { buf: [0.0; COMB_BUF_CAP], idx: 0, lp: 0.0 }
    }
}

struct Allpass {
    buf: [f32; ALLPASS_BUF_CAP],
    idx: usize,
}

impl Default for Allpass {
    fn default() -> Self {
        Self { buf: [0.0; ALLPASS_BUF_CAP], idx: 0 }
    }
}

struct ChannelState {
    combs: [Comb; 4],
    allpasses: [Allpass; 4],
    predelay_buf: Box<[f32; PREDELAY_MAX_SAMPLES]>,
    predelay_idx: usize,
    lfo_phase: f32,
}

impl Default for ChannelState {
    fn default() -> Self {
        Self {
            combs: Default::default(),
            allpasses: Default::default(),
            predelay_buf: Box::new([0.0; PREDELAY_MAX_SAMPLES]),
            predelay_idx: 0,
            lfo_phase: 0.0,
        }
    }
}

impl ChannelState {
    fn process_sample(
        &mut self,
        input: f32,
        sr: f32,
        size: f32,
        decay: f32,
        damp: f32,
        predelay_ms: f32,
        modulation: f32,
        lfo_inc: f32,
    ) -> f32 {
        // Pre-delay.
        let predelay_samples = ((predelay_ms * 0.001 * sr) as usize)
            .clamp(0, PREDELAY_MAX_SAMPLES - 1);
        self.predelay_buf[self.predelay_idx] = input;
        let pd_read = (self.predelay_idx + PREDELAY_MAX_SAMPLES - predelay_samples)
            % PREDELAY_MAX_SAMPLES;
        let pre_out = self.predelay_buf[pd_read];
        self.predelay_idx = (self.predelay_idx + 1) % PREDELAY_MAX_SAMPLES;

        // LFO for chorus-like modulation of the comb read positions.
        self.lfo_phase += lfo_inc;
        if self.lfo_phase >= core::f32::consts::TAU {
            self.lfo_phase -= core::f32::consts::TAU;
        }
        let mod_offset = (modulation * 8.0) * self.lfo_phase.sin();

        // 4 lowpass-feedback combs summed in parallel.
        let mut combs_out = 0.0;
        for (i, comb) in self.combs.iter_mut().enumerate() {
            let raw_len = (COMB_LENS[i] as f32 * size) as i32;
            let modulated_len = (raw_len + mod_offset as i32)
                .clamp(8, (COMB_BUF_CAP - 1) as i32) as usize;
            let read_idx = (comb.idx + COMB_BUF_CAP - modulated_len) % COMB_BUF_CAP;
            let read = comb.buf[read_idx];
            comb.lp = read * (1.0 - damp) + comb.lp * damp;
            comb.buf[comb.idx] = pre_out + comb.lp * decay;
            comb.idx = (comb.idx + 1) % COMB_BUF_CAP;
            combs_out += read;
        }
        let mut y = combs_out * 0.25;

        // 4 cascaded allpasses for diffusion.
        for (i, ap) in self.allpasses.iter_mut().enumerate() {
            let len = ((ALLPASS_LENS[i] as f32 * size) as usize)
                .clamp(8, ALLPASS_BUF_CAP - 1);
            let buf_val = ap.buf[ap.idx];
            let in_val = y;
            let out_val = -in_val + buf_val;
            ap.buf[ap.idx] = in_val + buf_val * 0.5;
            ap.idx = (ap.idx + 1) % len;
            y = out_val;
        }

        y
    }
}

// ===========================================================================
// CLAP wiring
// ===========================================================================

pub struct PluginShared {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: std::sync::atomic::AtomicBool,
}

impl PluginShared {
    pub fn new() -> Self {
        Self {
            params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
            bypass: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl Default for PluginShared {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    state: Box<ReverbState>,
    sample_rate: f32,
}

struct ReverbState {
    left: ChannelState,
    right: ChannelState,
}

impl ReverbState {
    fn new() -> Self {
        Self {
            left: ChannelState::default(),
            right: ChannelState::default(),
        }
    }
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    for event in events {
        let Some(pv) = event.as_event::<ParamValueEvent>() else { continue };
        let Some(id) = pv.param_id() else { continue };
        let i = id.get() as usize;
        if let Some(slot) = shared.params.get(i) {
            slot.store(pv.value() as f32, Ordering::Relaxed);
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
        Ok(Self {
            shared,
            state: Box::new(ReverbState::new()),
            sample_rate: audio_config.sample_rate as f32,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        apply_param_events(self.shared, events.input);

        let size       = self.shared.params[P_SIZE].load(Ordering::Relaxed);
        let decay      = self.shared.params[P_DECAY].load(Ordering::Relaxed);
        let damp       = self.shared.params[P_DAMP].load(Ordering::Relaxed);
        let predelay   = self.shared.params[P_PREDELAY].load(Ordering::Relaxed);
        let modulation = self.shared.params[P_MOD].load(Ordering::Relaxed);
        let width      = self.shared.params[P_WIDTH].load(Ordering::Relaxed);
        let mix        = self.shared.params[P_MIX].load(Ordering::Relaxed);
        let bypassed   = self.shared.bypass.load(Ordering::Relaxed);

        let sr = self.sample_rate;
        // ~0.6 Hz LFO for modulation, plus a tiny per-channel detune.
        let lfo_inc_l = core::f32::consts::TAU * 0.6 / sr;
        let lfo_inc_r = core::f32::consts::TAU * 0.73 / sr;

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            for (ch_idx, channel_pair) in channel_pairs.into_iter().enumerate() {
                // Left state for ch 0 and mono, right for ch ≥ 1.
                let state = if ch_idx == 0 {
                    &mut self.state.left
                } else {
                    &mut self.state.right
                };
                let lfo_inc = if ch_idx == 0 { lfo_inc_l } else { lfo_inc_r };
                match channel_pair {
                    ChannelPair::InputOutput(input, output) => {
                        if bypassed {
                            for (i, o) in input.iter().zip(output.iter_mut()) {
                                *o = *i;
                            }
                            continue;
                        }
                        for (i, o) in input.iter().zip(output.iter_mut()) {
                            let wet = state.process_sample(
                                *i, sr, size, decay, damp, predelay, modulation, lfo_inc,
                            );
                            // Width: blend wet with dry. Width=1 → full wet,
                            // width=0 → dry only on wet channel (mono reverb).
                            let wet_scaled = wet * width;
                            *o = *i * (1.0 - mix) + wet_scaled * mix;
                        }
                    }
                    ChannelPair::InPlace(buf) => {
                        if bypassed {
                            continue;
                        }
                        for s in buf.iter_mut() {
                            let dry = *s;
                            let wet = state.process_sample(
                                dry, sr, size, decay, damp, predelay, modulation, lfo_inc,
                            );
                            let wet_scaled = wet * width;
                            *s = dry * (1.0 - mix) + wet_scaled * mix;
                        }
                    }
                    ChannelPair::OutputOnly(buf) => buf.fill(0.0),
                    ChannelPair::InputOnly(_) => {}
                }
            }
        }

        Ok(ProcessStatus::Continue)
    }
}

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

    fn get_info(&mut self, param_index: u32, info: &mut ParamInfoWriter) {
        let Some(p) = PARAMS.get(param_index as usize) else { return };
        info.set(&ParamInfo {
            id: ClapId::new(p.id),
            flags: ParamInfoFlags::IS_AUTOMATABLE,
            cookie: Default::default(),
            name: p.name,
            module: b"",
            min_value: p.min,
            max_value: p.max,
            default_value: p.default,
        });
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
        let Some(p) = pdef(param_id.get()) else { return Ok(()) };
        if p.unit.is_empty() {
            write!(writer, "{:.2}", value)
        } else {
            write!(writer, "{:.2} {}", value, p.unit)
        }
    }

    fn text_to_value(&mut self, param_id: ClapId, text: &CStr) -> Option<f64> {
        let p = pdef(param_id.get())?;
        let s = text.to_str().ok()?.trim();
        let s = if !p.unit.is_empty() {
            s.strip_suffix(p.unit).unwrap_or(s).trim()
        } else { s };
        s.parse::<f64>().ok().map(|v| v.clamp(p.min, p.max))
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
            .register::<PluginParams>();
    }
}

impl DefaultPluginFactory for SuperDuperReverb {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("co.superduperai.reverb", "SuperDuper Reverb")
            .with_vendor("SuperDuperAI")
            .with_version("0.1.0")
            .with_description("Stereo plate reverb")
            .with_features([AUDIO_EFFECT, STEREO, REVERB])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        Ok(PluginShared::new())
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        shared: &'a PluginShared,
    ) -> Result<PluginMainThread<'a>, PluginError> {
        Ok(PluginMainThread { shared })
    }
}

clack_export_entry!(SinglePluginEntry<SuperDuperReverb>);
