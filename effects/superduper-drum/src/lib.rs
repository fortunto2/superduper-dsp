//! SuperDuper Drum — analog-synthesis drum machine.
//!
//! Six voices (Kick, Snare, Hat Closed, Hat Open, Clap, Cowbell) all
//! synthesised — no samples, no fixtures. Each voice has its own
//! tiny synthesis recipe inside `voices.rs`; this file is the CLAP
//! glue: param table, MIDI routing, voice trigger logic, audio mixdown.
//!
//! ## Ecosystem integration: note passthrough
//!
//! Standard MIDI drum mapping (General MIDI Percussion) triggers the
//! drum voices when notes 35-57 land on the input port. Anything
//! OUTSIDE that range — typical bass notes around C2 = MIDI 36... wait,
//! that's a kick — outside-drum-map notes get forwarded to the CLAP
//! note output port. The user routes that output into Wave or Kubyz
//! on another track and gets bass synth fired by the same MIDI clip
//! that drives the drums.
//!
//! Recommended pattern in REAPER: drum MIDI item on this plugin,
//! route plugin's note output to a second track holding Wave with
//! its own bass MIDI on a higher octave. The drum kicks + the wave
//! bassline lock together because they share the same clip.

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod presets;
pub mod voices;

use atomic_float::AtomicF32;
use clack_common::events::{Match, Pckn};
use clack_common::events::event_types::{NoteOffEvent, NoteOnEvent};
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

use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use clack_plugin::events::spaces::CoreEventSpace;
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use superduper_dsp_sdk::clap_helpers::{output_slice, ParamDef};
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};

use voices::{Clap as ClapVoice, Cowbell, DrumParams, HiHat, Kick, Snare, VoiceKind, note_to_voice};

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn init_logging() { superduper_dsp_sdk::log::init("drum"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Param table — 4 params per voice × 6 voices = 24, plus 3 master = 27.
// Voice ordering matches VoiceKind so we can index params[base+offset].
// ---------------------------------------------------------------------------

pub const PARAMS_PER_VOICE: usize = 4;
const VOICE_NAMES: [&[u8]; 6] = [b"Kick", b"Snare", b"HHc", b"HHo", b"Clap", b"Cowb"];

const fn pidx(voice: usize, offset: usize) -> u32 { (voice * PARAMS_PER_VOICE + offset) as u32 }

pub const PARAMS: &[ParamDef] = &[
    // Kick
    ParamDef { id: pidx(0, 0), name: b"Kick Tune",   min: -24.0, max: 24.0, default: 0.0,  unit: "ST" },
    ParamDef { id: pidx(0, 1), name: b"Kick Decay",  min: 0.05,  max: 1.5,  default: 0.42, unit: "s"  },
    ParamDef { id: pidx(0, 2), name: b"Kick Level",  min: 0.0,   max: 1.0,  default: 0.9,  unit: ""   },
    ParamDef { id: pidx(0, 3), name: b"Kick Pan",    min: -1.0,  max: 1.0,  default: 0.0,  unit: ""   },
    // Snare
    ParamDef { id: pidx(1, 0), name: b"Snare Tune",  min: -24.0, max: 24.0, default: 0.0,  unit: "ST" },
    ParamDef { id: pidx(1, 1), name: b"Snare Decay", min: 0.05,  max: 1.0,  default: 0.18, unit: "s"  },
    ParamDef { id: pidx(1, 2), name: b"Snare Level", min: 0.0,   max: 1.0,  default: 0.75, unit: ""   },
    ParamDef { id: pidx(1, 3), name: b"Snare Pan",   min: -1.0,  max: 1.0,  default: 0.0,  unit: ""   },
    // Hat Closed
    ParamDef { id: pidx(2, 0), name: b"HHc Tune",    min: -24.0, max: 24.0, default: 0.0,  unit: "ST" },
    ParamDef { id: pidx(2, 1), name: b"HHc Decay",   min: 0.02,  max: 0.5,  default: 0.08, unit: "s"  },
    ParamDef { id: pidx(2, 2), name: b"HHc Level",   min: 0.0,   max: 1.0,  default: 0.55, unit: ""   },
    ParamDef { id: pidx(2, 3), name: b"HHc Pan",     min: -1.0,  max: 1.0,  default: 0.3,  unit: ""   },
    // Hat Open
    ParamDef { id: pidx(3, 0), name: b"HHo Tune",    min: -24.0, max: 24.0, default: 0.0,  unit: "ST" },
    ParamDef { id: pidx(3, 1), name: b"HHo Decay",   min: 0.1,   max: 1.5,  default: 0.45, unit: "s"  },
    ParamDef { id: pidx(3, 2), name: b"HHo Level",   min: 0.0,   max: 1.0,  default: 0.5,  unit: ""   },
    ParamDef { id: pidx(3, 3), name: b"HHo Pan",     min: -1.0,  max: 1.0,  default: 0.3,  unit: ""   },
    // Clap
    ParamDef { id: pidx(4, 0), name: b"Clap Tune",   min: -24.0, max: 24.0, default: 0.0,  unit: "ST" },
    ParamDef { id: pidx(4, 1), name: b"Clap Decay",  min: 0.05,  max: 1.0,  default: 0.22, unit: "s"  },
    ParamDef { id: pidx(4, 2), name: b"Clap Level",  min: 0.0,   max: 1.0,  default: 0.7,  unit: ""   },
    ParamDef { id: pidx(4, 3), name: b"Clap Pan",    min: -1.0,  max: 1.0,  default: -0.2, unit: ""   },
    // Cowbell
    ParamDef { id: pidx(5, 0), name: b"Cowb Tune",   min: -24.0, max: 24.0, default: 0.0,  unit: "ST" },
    ParamDef { id: pidx(5, 1), name: b"Cowb Decay",  min: 0.05,  max: 1.5,  default: 0.4,  unit: "s"  },
    ParamDef { id: pidx(5, 2), name: b"Cowb Level",  min: 0.0,   max: 1.0,  default: 0.55, unit: ""   },
    ParamDef { id: pidx(5, 3), name: b"Cowb Pan",    min: -1.0,  max: 1.0,  default: 0.2,  unit: ""   },
    // Master
    ParamDef { id: 24, name: b"Drive",   min: 0.0,   max: 1.0,   default: 0.0,  unit: ""   },
    ParamDef { id: 25, name: b"Master",  min: -36.0, max: 6.0,   default: -8.0, unit: "dB" },
    // Note Passthrough toggle — when on, MIDI notes outside the drum
    // map (35-57 GM percussion range) are forwarded to the CLAP note
    // output so chained synths can be played from the same MIDI clip.
    ParamDef { id: 26, name: b"Note Out", min: 0.0,   max: 1.0,   default: 1.0,  unit: ""   },
];

pub const fn voice_param_idx(voice: usize, offset: usize) -> usize {
    voice * PARAMS_PER_VOICE + offset
}

pub const P_DRIVE: usize = 24;
pub const P_MASTER: usize = 25;
pub const P_NOTE_OUT: usize = 26;

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
    /// Per-voice "currently firing" flag for the GUI pad-light strip.
    /// Atomically pulsed on trigger and decayed by the GUI.
    pub voice_pulse: [AtomicF32; 6],
    /// GUI → audio thread trigger bridge. The pads in the GUI set a
    /// non-zero velocity here (8-bit, fits in an AtomicF32) and the
    /// audio thread reads + zeros it at the top of every process()
    /// block, firing the corresponding voice. Atomic instead of a
    /// channel so we don't allocate or block on the audio thread.
    pub voice_trigger_request: [AtomicF32; 6],
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
                voice_pulse: std::array::from_fn(|_| AtomicF32::new(0.0)),
                voice_trigger_request: std::array::from_fn(|_| AtomicF32::new(0.0)),
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
    kick: Kick,
    snare: Snare,
    hat_closed: HiHat,
    hat_open: HiHat,
    clap_voice: ClapVoice,
    cowbell: Cowbell,
    sample_rate: f32,
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
        slog!("drum activate sr={}", audio_config.sample_rate);
        Ok(Self {
            shared,
            kick: Kick::default(),
            snare: Snare::default(),
            hat_closed: HiHat::default(),
            hat_open: HiHat::default(),
            clap_voice: ClapVoice::default(),
            cowbell: Cowbell::default(),
            sample_rate: audio_config.sample_rate as f32,
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
        let note_passthrough = self.shared.params[P_NOTE_OUT].load(Ordering::Relaxed) >= 0.5;

        // Drain GUI-triggered hits — the pads in the GUI write a
        // non-zero velocity here when clicked. We zero each slot
        // after firing so the next click re-arms it.
        for (idx, slot) in self.shared.voice_trigger_request.iter().enumerate() {
            let vel = slot.swap(0.0, Ordering::AcqRel);
            if vel > 0.0 {
                let voice = match idx {
                    0 => VoiceKind::Kick,
                    1 => VoiceKind::Snare,
                    2 => VoiceKind::HatClosed,
                    3 => VoiceKind::HatOpen,
                    4 => VoiceKind::Clap,
                    5 => VoiceKind::Cowbell,
                    _ => continue,
                };
                self.trigger(voice, vel.clamp(0.0, 1.0));
            }
        }

        // Walk the event stream — drum-map keys trigger voices, anything
        // else passes through to the note output (when enabled). We
        // process events between renders so each block is one batch.
        for batch in events.input.batch() {
            for ev in batch.events() {
                if let Some(core) = ev.as_core_event() {
                    match core {
                        CoreEventSpace::NoteOn(n) => {
                            let key = match n.key() {
                                Match::Specific(k) => k as u8,
                                Match::All => {
                                    slog!("rx NoteOn key=All vel={:.2} → ignored", n.velocity());
                                    continue;
                                }
                            };
                            let velocity = n.velocity().clamp(0.0, 1.0) as f32;
                            let voice = note_to_voice(key);
                            slog!(
                                "rx NoteOn(clap) key={} vel={:.2} → {}",
                                key, velocity,
                                match voice {
                                    Some(v) => format!("trigger {:?}", v),
                                    None if note_passthrough => "passthrough".into(),
                                    None => "ignored (no map, passthrough off)".into(),
                                }
                            );
                            if let Some(voice) = voice {
                                self.trigger(voice, velocity);
                            } else if note_passthrough {
                                // Forward — same timing, same channel, same key.
                                let fwd = NoteOnEvent::new(
                                    n.header().time(),
                                    Pckn::new(0u16, 0u16, key as u16, 0u32),
                                    velocity as f64,
                                );
                                let _ = events.output.try_push(&fwd);
                            }
                        }
                        CoreEventSpace::NoteOff(n) => {
                            // Drums don't sustain — only forward off events
                            // for non-drum keys so chained synths release.
                            let key = match n.key() {
                                Match::Specific(k) => k as u8,
                                _ => continue,
                            };
                            if note_to_voice(key).is_none() && note_passthrough {
                                let fwd = NoteOffEvent::new(
                                    n.header().time(),
                                    Pckn::new(0u16, 0u16, key as u16, 0u32),
                                    1.0,
                                );
                                let _ = events.output.try_push(&fwd);
                            }
                        }
                        CoreEventSpace::Midi(m) => {
                            // Raw MIDI 1.0 path. Status 0x90 = NoteOn,
                            // 0x80 = NoteOff. Anything else (CC, PB, …)
                            // passes through verbatim.
                            let data = m.data();
                            let status = data[0] & 0xF0;
                            slog!("rx MIDI status=0x{:02X} d1={} d2={}", status, data[1], data[2]);
                            match status {
                                0x90 if data[2] > 0 => {
                                    let key = data[1];
                                    let vel = data[2] as f32 / 127.0;
                                    if let Some(voice) = note_to_voice(key) {
                                        self.trigger(voice, vel);
                                    } else if note_passthrough {
                                        let fwd = NoteOnEvent::new(
                                            m.header().time(),
                                            Pckn::new(0u16, 0u16, key as u16, 0u32),
                                            vel as f64,
                                        );
                                        let _ = events.output.try_push(&fwd);
                                    }
                                }
                                0x90 | 0x80 => {
                                    let key = data[1];
                                    if note_to_voice(key).is_none() && note_passthrough {
                                        let fwd = NoteOffEvent::new(
                                            m.header().time(),
                                            Pckn::new(0u16, 0u16, key as u16, 0u32),
                                            1.0,
                                        );
                                        let _ = events.output.try_push(&fwd);
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        // Now render audio for the whole block. Drums don't react to
        // events mid-block (one-shot voices), so a single render pass
        // per block is correct + cheap.
        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            let mut writers: Vec<_> = channel_pairs
                .into_iter()
                .filter_map(output_slice)
                .collect();
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

            self.render_block(out_l, out_r, frames, sr);
        }

        Ok(ProcessStatus::Continue)
    }
}

impl<'a> PluginAudioProcessor<'a> {
    fn trigger(&mut self, voice: VoiceKind, velocity: f32) {
        match voice {
            VoiceKind::Kick => self.kick.trigger(velocity),
            VoiceKind::Snare => self.snare.trigger(velocity),
            VoiceKind::HatClosed => {
                // Closing the open hat when a closed hat fires — classic
                // 808 choke group behaviour.
                self.hat_open = HiHat::default();
                self.hat_closed.trigger(velocity);
            }
            VoiceKind::HatOpen => self.hat_open.trigger(velocity),
            VoiceKind::Clap => self.clap_voice.trigger(velocity),
            VoiceKind::Cowbell => self.cowbell.trigger(velocity),
        }
        self.shared.voice_pulse[voice as usize].store(1.0, Ordering::Relaxed);
    }

    fn render_block(&mut self, out_l: &mut [f32], out_r: &mut [f32], n: usize, sr: f32) {
        let load = |i: usize| self.shared.params[i].load(Ordering::Relaxed);
        let p_kick = DrumParams {
            tune_st: load(voice_param_idx(0, 0)),
            decay_s: load(voice_param_idx(0, 1)),
            level: load(voice_param_idx(0, 2)),
            pan: load(voice_param_idx(0, 3)),
        };
        let p_snare = DrumParams {
            tune_st: load(voice_param_idx(1, 0)),
            decay_s: load(voice_param_idx(1, 1)),
            level: load(voice_param_idx(1, 2)),
            pan: load(voice_param_idx(1, 3)),
        };
        let p_hhc = DrumParams {
            tune_st: load(voice_param_idx(2, 0)),
            decay_s: load(voice_param_idx(2, 1)),
            level: load(voice_param_idx(2, 2)),
            pan: load(voice_param_idx(2, 3)),
        };
        let p_hho = DrumParams {
            tune_st: load(voice_param_idx(3, 0)),
            decay_s: load(voice_param_idx(3, 1)),
            level: load(voice_param_idx(3, 2)),
            pan: load(voice_param_idx(3, 3)),
        };
        let p_clap = DrumParams {
            tune_st: load(voice_param_idx(4, 0)),
            decay_s: load(voice_param_idx(4, 1)),
            level: load(voice_param_idx(4, 2)),
            pan: load(voice_param_idx(4, 3)),
        };
        let p_cow = DrumParams {
            tune_st: load(voice_param_idx(5, 0)),
            decay_s: load(voice_param_idx(5, 1)),
            level: load(voice_param_idx(5, 2)),
            pan: load(voice_param_idx(5, 3)),
        };
        let drive = self.shared.params[P_DRIVE].load(Ordering::Relaxed);
        let master_db = self.shared.params[P_MASTER].load(Ordering::Relaxed);
        let master_lin = 10f32.powf(master_db / 20.0);

        for i in 0..n {
            let mut l = 0.0_f32;
            let mut r = 0.0_f32;

            // Helper: mix a voice's mono sample into L/R with constant-
            // power-ish pan (-1..+1).
            #[inline]
            fn pan_mix(sample: f32, pan: f32, l: &mut f32, r: &mut f32) {
                let p = pan.clamp(-1.0, 1.0);
                let lg = (1.0 - p).max(0.0).min(1.0);
                let rg = (1.0 + p).max(0.0).min(1.0);
                *l += sample * lg.sqrt();
                *r += sample * rg.sqrt();
            }

            pan_mix(self.kick.process(sr, p_kick), p_kick.pan, &mut l, &mut r);
            pan_mix(self.snare.process(sr, p_snare), p_snare.pan, &mut l, &mut r);
            pan_mix(self.hat_closed.process(sr, p_hhc), p_hhc.pan, &mut l, &mut r);
            pan_mix(self.hat_open.process(sr, p_hho), p_hho.pan, &mut l, &mut r);
            pan_mix(self.clap_voice.process(sr, p_clap), p_clap.pan, &mut l, &mut r);
            pan_mix(self.cowbell.process(sr, p_cow), p_cow.pan, &mut l, &mut r);

            // Drive — soft saturation on the bus for that "808 through
            // an SP-1200" colour.
            if drive > 0.001 {
                let g = 1.0 + drive * 3.0;
                l = (l * g).tanh();
                r = (r * g).tanh();
            }

            out_l[i] = l * master_lin;
            out_r[i] = r * master_lin;
            self.shared.scope.push((out_l[i] + out_r[i]) * 0.5);
        }
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 { if is_input { 0 } else { 1 } }
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

impl PluginNotePortsImpl for PluginMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 { 1 }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if index != 0 { return; }
        writer.set(&NotePortInfo {
            id: ClapId::new(0),
            name: if is_input { b"MIDI In" } else { b"Bass / Pass-thru" },
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
        if id.get() as usize == P_NOTE_OUT {
            return write!(w, "{}", if v >= 0.5 { "On" } else { "Off" });
        }
        ParamDef::write_display(PARAMS, id, v, w)
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        ParamDef::parse_text(PARAMS, id, t)
    }
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
    }
}

superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);

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

pub struct SuperDuperDrum;

impl Plugin for SuperDuperDrum {
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

impl DefaultPluginFactory for SuperDuperDrum {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("co.superduperai.drum", plugin_display_name!("SuperDuper Drum"))
            .with_vendor("SuperDuperAI")
            .with_version(version_string!("0.2"))
            .with_description("Analog-synthesis drum machine — 6 voices, MIDI-driven, note passthrough for chained synths")
            .with_features([INSTRUMENT, STEREO, DRUM_MACHINE])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Drum — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperDrum>);

/// Pub re-exports for GUI / tests to reach voice names.
pub fn voice_name(i: usize) -> &'static str {
    match i {
        0 => "Kick", 1 => "Snare", 2 => "HH Closed",
        3 => "HH Open", 4 => "Clap", 5 => "Cowbell",
        _ => "?",
    }
}

pub fn voice_short(i: usize) -> &'static str {
    std::str::from_utf8(VOICE_NAMES[i]).unwrap_or("?")
}
