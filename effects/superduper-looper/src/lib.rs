//! SuperDuper Looper — Mobius-style live looper for jam / build-up
//! workflows.
//!
//! Four independent loop tracks. Audio in → record + play. Per-track
//! Rec / Play-Stop / Overdub / Clear from the GUI or from MIDI CCs
//! (one per command per track — see README for the map). Loop length
//! auto-quantises to whole bars from host BPM when Sync is on, free-
//! form when off.
//!
//! Audio thread invariants: no heap, all buffers pre-allocated at
//! activate(max_frames_count + 60 sec headroom per track), state
//! transitions driven by atomic commands so the GUI never blocks.

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod track;

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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use parking_lot::Mutex;
use superduper_dsp_sdk::clap_helpers::{split_io, ParamDef};
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};

use track::{LoopTrack, TrackCommand, TrackState, MAX_LAYERS};

pub const TRACK_COUNT: usize = 4;
// 4 tracks × MAX_LAYERS layer buffers × this many seconds bounds the RAM
// (≈ TRACK_COUNT·MAX_LAYERS·sec·sr·2·4 bytes). 30 s keeps it sane for layers.
pub const MAX_LOOP_SECONDS: f32 = 30.0;
/// Sentinel for "no layer is recording" in the shared recording_layer surface.
pub const NO_LAYER: u32 = u32::MAX;

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn init_logging() { superduper_dsp_sdk::log::init("looper"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Param table — global + per-track level/feedback/mute
// ---------------------------------------------------------------------------

const PARAMS_PER_TRACK: usize = 3;
const GLOBAL_PARAM_COUNT: usize = 4;

const fn pidx(track: usize, offset: usize) -> u32 {
    (GLOBAL_PARAM_COUNT + track * PARAMS_PER_TRACK + offset) as u32
}

pub const PARAMS: &[ParamDef] = &[
    // ----- Globals -----
    ParamDef { id: 0, name: b"Sync",     min: 0.0, max: 1.0,  default: 1.0, unit: "" },
    // Bars to quantise loop length when Sync is on. 0 = auto (locks
    // to the next bar boundary once the user hits Rec a second time).
    ParamDef { id: 1, name: b"Bars",     min: 0.0, max: 16.0, default: 0.0, unit: "" },
    ParamDef { id: 2, name: b"Dry",      min: 0.0, max: 1.0,  default: 1.0, unit: "" },
    ParamDef { id: 3, name: b"Master",   min: -36.0, max: 6.0, default: 0.0, unit: "dB" },
    // ----- Per-track (4 × 3) -----
    ParamDef { id: pidx(0, 0), name: b"T1 Level",    min: 0.0, max: 1.5, default: 1.0, unit: "" },
    ParamDef { id: pidx(0, 1), name: b"T1 Feedback", min: 0.0, max: 1.0, default: 1.0, unit: "" },
    ParamDef { id: pidx(0, 2), name: b"T1 Mute",     min: 0.0, max: 1.0, default: 0.0, unit: "" },
    ParamDef { id: pidx(1, 0), name: b"T2 Level",    min: 0.0, max: 1.5, default: 1.0, unit: "" },
    ParamDef { id: pidx(1, 1), name: b"T2 Feedback", min: 0.0, max: 1.0, default: 1.0, unit: "" },
    ParamDef { id: pidx(1, 2), name: b"T2 Mute",     min: 0.0, max: 1.0, default: 0.0, unit: "" },
    ParamDef { id: pidx(2, 0), name: b"T3 Level",    min: 0.0, max: 1.5, default: 1.0, unit: "" },
    ParamDef { id: pidx(2, 1), name: b"T3 Feedback", min: 0.0, max: 1.0, default: 1.0, unit: "" },
    ParamDef { id: pidx(2, 2), name: b"T3 Mute",     min: 0.0, max: 1.0, default: 0.0, unit: "" },
    ParamDef { id: pidx(3, 0), name: b"T4 Level",    min: 0.0, max: 1.5, default: 1.0, unit: "" },
    ParamDef { id: pidx(3, 1), name: b"T4 Feedback", min: 0.0, max: 1.0, default: 1.0, unit: "" },
    ParamDef { id: pidx(3, 2), name: b"T4 Mute",     min: 0.0, max: 1.0, default: 0.0, unit: "" },
];

/// Params that are discrete: enums, booleans, the preset selector. Declared to
/// the host with IS_STEPPED so it quantises automation instead of sweeping
/// through the intermediate values — a ramp across a preset selector otherwise
/// recalls every kit between the two endpoints.
const STEPPED_PARAMS: &[u32] = &[0];

pub const P_SYNC: usize = 0;
pub const P_BARS: usize = 1;
pub const P_DRY: usize = 2;
pub const P_MASTER: usize = 3;

pub const fn track_level_idx(t: usize) -> usize {
    GLOBAL_PARAM_COUNT + t * PARAMS_PER_TRACK
}
pub const fn track_fb_idx(t: usize) -> usize {
    GLOBAL_PARAM_COUNT + t * PARAMS_PER_TRACK + 1
}
pub const fn track_mute_idx(t: usize) -> usize {
    GLOBAL_PARAM_COUNT + t * PARAMS_PER_TRACK + 2
}

// ---------------------------------------------------------------------------
// MIDI mapping — one CC per (track, command). User can re-map via
// the DAW's CC learn if they want a different controller.
// ---------------------------------------------------------------------------

/// Maps an incoming MIDI CC number to (track, command). 20-31 is a
/// "spare" CC block in the GM spec — least likely to clash with
/// other plugins' default mappings. 4 tracks × 4 commands fits.
fn midi_cc_to_command(cc: u8) -> Option<(usize, TrackCommand)> {
    let (track, cmd_idx) = match cc {
        20..=23 => (cc as usize - 20, 1), // Rec
        24..=27 => (cc as usize - 24, 2), // Play/Stop
        28..=31 => (cc as usize - 28, 3), // Overdub
        32..=35 => (cc as usize - 32, 5), // Undo
        // Block bumped — Clear lives at 60-63 to keep destructive
        // commands away from accidental presses.
        60..=63 => (cc as usize - 60, 4), // Clear
        _ => return None,
    };
    if track >= TRACK_COUNT { return None; }
    let cmd = match cmd_idx {
        1 => TrackCommand::Rec,
        2 => TrackCommand::PlayStop,
        3 => TrackCommand::Overdub,
        4 => TrackCommand::Clear,
        5 => TrackCommand::Undo,
        _ => return None,
    };
    Some((track, cmd))
}

// ---------------------------------------------------------------------------
// Shared params
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
    /// Currently-selected preset index — persisted via simple_state.
    pub active_preset: std::sync::atomic::AtomicU32,
    /// Track state surface for the GUI — atomically updated by the
    /// audio thread on every transition.
    pub track_state: [AtomicU32; TRACK_COUNT],
    /// Track progress 0..1 for GUI progress bars. Updated cheaply
    /// once per block on the audio thread.
    pub track_progress: [AtomicF32; TRACK_COUNT],
    /// Cached host BPM from TransportEvent — used by Sync mode.
    pub host_bpm: AtomicF32,
    /// Track command submission atoms — set by GUI / MIDI handlers,
    /// drained by the audio thread.
    pub track_command: [AtomicU32; TRACK_COUNT],
    /// Number of finalised layers per track (for the GUI layer stack).
    pub layer_count: [AtomicU32; TRACK_COUNT],
    /// The layer index currently being recorded (or NO_LAYER) — GUI pulses it.
    pub recording_layer: [AtomicU32; TRACK_COUNT],
    /// Per-layer volume 0..1.5 — GUI writes, audio thread reads for the mix.
    pub layer_volume: [[AtomicF32; MAX_LAYERS]; TRACK_COUNT],
}

pub struct PluginShared { pub inner: SharedParams }

impl PluginShared {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| AtomicBool::new(false)),
                active_preset: std::sync::atomic::AtomicU32::new(0),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
                track_state: std::array::from_fn(|_| AtomicU32::new(0)),
                track_progress: std::array::from_fn(|_| AtomicF32::new(0.0)),
                host_bpm: AtomicF32::new(120.0),
                track_command: std::array::from_fn(|_| AtomicU32::new(0)),
                layer_count: std::array::from_fn(|_| AtomicU32::new(0)),
                recording_layer: std::array::from_fn(|_| AtomicU32::new(NO_LAYER)),
                layer_volume: std::array::from_fn(|_| std::array::from_fn(|_| AtomicF32::new(1.0))),
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

/// GUI helper: submit a track command via the shared atomic so the
/// audio thread picks it up on the next process() block.
pub fn submit_track_command(shared: &SharedParamsInner, track: usize, cmd: TrackCommand) {
    if track < TRACK_COUNT {
        shared.track_command[track].store(cmd as u32, Ordering::Release);
    }
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
    tracks: [LoopTrack; TRACK_COUNT],
    sample_rate: f32,
    /// When sync is on, recording auto-stops at the end of the next
    /// whole bar. We track that here so the second Rec press just
    /// arms the auto-stop instead of locking immediately.
    pending_quantize_stop: [Option<usize>; TRACK_COUNT],
    /// Scratch for the mono path, sized once at activate(). It used to `vec!`
    /// two buffers per block — two mallocs and two frees inside the audio
    /// callback, on a live looper, where a dropout gets recorded into the loop
    /// and stays there.
    mono_in: Box<[f32]>,
    mono_out: Box<[f32]>,
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
        slog!("looper activate sr={}", sr);
        let cap = (sr * MAX_LOOP_SECONDS) as usize;
        let max_frames = cfg.max_frames_count as usize;
        Ok(Self {
            shared,
            tracks: std::array::from_fn(|_| LoopTrack::new(cap)),
            sample_rate: sr,
            pending_quantize_stop: [None; TRACK_COUNT],
            mono_in: vec![0.0; max_frames].into_boxed_slice(),
            mono_out: vec![0.0; max_frames].into_boxed_slice(),
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
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, events.input);
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(
            &self.shared.params, &self.shared.dirty_params, events.output);
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(
            &self.shared.gesture_begin, &self.shared.gesture_end, events.output);

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        let sr = self.sample_rate;

        // Drain transport + MIDI from the event stream.
        for ev in events.input {
            if let Some(core) = ev.as_core_event() {
                match core {
                    CoreEventSpace::Transport(t) => {
                        self.shared.host_bpm.store(t.tempo as f32, Ordering::Relaxed);
                    }
                    CoreEventSpace::Midi(m) => {
                        let d = m.data();
                        if (d[0] & 0xF0) == 0xB0 {  // CC
                            if let Some((track, cmd)) = midi_cc_to_command(d[1]) {
                                // Bottom half of CC range = release;
                                // only fire on the press half.
                                if d[2] >= 64 {
                                    submit_track_command(&self.shared.inner, track, cmd);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Process commands now that all submissions for this block landed.
        let sync_on = self.shared.params[P_SYNC].load(Ordering::Relaxed) >= 0.5;
        let bars_target = self.shared.params[P_BARS].load(Ordering::Relaxed).round() as u32;
        let bpm = self.shared.host_bpm.load(Ordering::Relaxed).max(20.0);
        // 1 bar = 4 beats × (60 / bpm) seconds × sr samples.
        let bar_samples = (sr * 60.0 / bpm * 4.0) as usize;
        for t in 0..TRACK_COUNT {
            // GUI / MIDI atoms first, then per-track buttons.
            let gui_cmd = TrackCommand::from_u32(
                self.shared.track_command[t].swap(0, Ordering::AcqRel)
            );
            let buf_cmd = self.tracks[t].take_command();
            for cmd in [gui_cmd, buf_cmd] {
                if cmd != TrackCommand::None {
                    self.apply_command(t, cmd, sync_on, bars_target, bar_samples);
                }
            }
        }

        // Audio render.
        let dry_gain = self.shared.params[P_DRY].load(Ordering::Relaxed);
        let master_db = self.shared.params[P_MASTER].load(Ordering::Relaxed);
        let master_lin = 10f32.powf(master_db / 20.0);
        let levels = [
            self.shared.params[track_level_idx(0)].load(Ordering::Relaxed),
            self.shared.params[track_level_idx(1)].load(Ordering::Relaxed),
            self.shared.params[track_level_idx(2)].load(Ordering::Relaxed),
            self.shared.params[track_level_idx(3)].load(Ordering::Relaxed),
        ];
        let feedbacks = [
            self.shared.params[track_fb_idx(0)].load(Ordering::Relaxed),
            self.shared.params[track_fb_idx(1)].load(Ordering::Relaxed),
            self.shared.params[track_fb_idx(2)].load(Ordering::Relaxed),
            self.shared.params[track_fb_idx(3)].load(Ordering::Relaxed),
        ];
        let mutes = [
            self.shared.params[track_mute_idx(0)].load(Ordering::Relaxed) >= 0.5,
            self.shared.params[track_mute_idx(1)].load(Ordering::Relaxed) >= 0.5,
            self.shared.params[track_mute_idx(2)].load(Ordering::Relaxed) >= 0.5,
            self.shared.params[track_mute_idx(3)].load(Ordering::Relaxed) >= 0.5,
        ];

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            let mut iter = channel_pairs.into_iter();
            let Some(ch_l) = iter.next() else { continue };
            let ch_r = iter.next();
            let Some((l_read, l_write)) = split_io(ch_l) else { continue };
            let r = ch_r.and_then(split_io);

            if bypassed {
                l_write.copy_from_slice(l_read);
                if let Some((rr, rw)) = r { rw.copy_from_slice(rr); }
                continue;
            }

            match r {
                Some((r_read, r_write)) => {
                    self.render_stereo(l_read, l_write, r_read, r_write,
                        dry_gain, master_lin, &levels, &feedbacks, &mutes,
                        sync_on, bars_target, bar_samples);
                }
                None => {
                    self.render_mono(l_read, l_write,
                        dry_gain, master_lin, &levels, &feedbacks, &mutes,
                        sync_on, bars_target, bar_samples);
                }
            }
        }

        // Publish progress + state surface for the GUI.
        for t in 0..TRACK_COUNT {
            let tr = &self.tracks[t];
            let prog = if tr.length_frames == 0 { 0.0 }
                else { tr.cursor as f32 / tr.length_frames as f32 };
            self.shared.track_progress[t].store(prog, Ordering::Relaxed);
        }

        Ok(ProcessStatus::Continue)
    }
}

impl<'a> PluginAudioProcessor<'a> {
    fn apply_command(
        &mut self, t: usize, cmd: TrackCommand,
        sync_on: bool, bars_target: u32, bar_samples: usize,
    ) {
        let state = self.tracks[t].state();
        match cmd {
            TrackCommand::Rec => {
                match state {
                    TrackState::Empty | TrackState::Stopped => {
                        // Start fresh recording.
                        self.tracks[t].clear();
                        self.tracks[t].set_state(TrackState::Recording);
                    }
                    TrackState::Recording => {
                        // Second press locks the loop length.
                        if sync_on && bars_target > 0 {
                            // Hard length — fixed bars from now.
                            let target = (bars_target as usize) * bar_samples;
                            let cap = self.tracks[t].capacity();
                            self.tracks[t].length_frames = target.min(cap);
                            self.tracks[t].cursor = 0;
                            self.tracks[t].layer_count = 1; // layer 0 finalised
                            self.tracks[t].set_state(TrackState::Playing);
                        } else if sync_on {
                            // Auto-quantise to the NEXT bar boundary.
                            self.pending_quantize_stop[t] = Some(bar_samples);
                        } else {
                            // Free-form: lock at the current cursor.
                            self.tracks[t].length_frames = self.tracks[t].cursor.max(1);
                            self.tracks[t].cursor = 0;
                            self.tracks[t].layer_count = 1; // layer 0 finalised
                            self.tracks[t].set_state(TrackState::Playing);
                        }
                    }
                    _ => {} // Rec is a no-op while playing/overdubbing
                }
            }
            TrackCommand::PlayStop => {
                match state {
                    TrackState::Playing | TrackState::Overdubbing => {
                        self.tracks[t].set_state(TrackState::Stopped);
                    }
                    TrackState::Stopped => {
                        self.tracks[t].cursor = 0;
                        self.tracks[t].set_state(TrackState::Playing);
                    }
                    _ => {}
                }
            }
            TrackCommand::Overdub => {
                match state {
                    // Opens a fresh layer at `layer_count` (no-op if the stack is full).
                    TrackState::Playing => { self.tracks[t].begin_overdub(); }
                    // Commit the in-progress layer into the mix.
                    TrackState::Overdubbing => self.tracks[t].finalize_overdub(),
                    _ => {}
                }
            }
            TrackCommand::Clear => self.tracks[t].clear(),
            TrackCommand::Undo => self.tracks[t].undo(),
            TrackCommand::None => {}
        }
    }

    fn render_stereo(
        &mut self,
        l_read: &[f32], l_write: &mut [f32],
        r_read: &[f32], r_write: &mut [f32],
        dry_gain: f32, master_lin: f32,
        levels: &[f32; 4], feedbacks: &[f32; 4], mutes: &[bool; 4],
        _sync_on: bool, _bars_target: u32, _bar_samples: usize,
    ) {
        let n = l_read.len().min(r_read.len()).min(l_write.len()).min(r_write.len());
        for i in 0..n {
            let dry_l = l_read[i];
            let dry_r = r_read[i];
            let mut wet_l = 0.0_f32;
            let mut wet_r = 0.0_f32;

            for t in 0..TRACK_COUNT {
                let tr = &mut self.tracks[t];
                let state = tr.state();
                let cap = tr.capacity();
                let level = levels[t];
                let fb = feedbacks[t];
                let mute = mutes[t];

                match state {
                    TrackState::Recording => {
                        // Append to buffer; auto-stop on quantize-pending
                        // when we reach the next bar boundary.
                        if tr.cursor < cap {
                            tr.layers[0][tr.cursor * 2] = dry_l;
                            tr.layers[0][tr.cursor * 2 + 1] = dry_r;
                            tr.cursor += 1;
                            tr.length_frames = tr.cursor;
                        }
                        if let Some(bar) = self.pending_quantize_stop[t] {
                            // Snap when cursor crosses a bar boundary.
                            if tr.cursor.is_multiple_of(bar) && tr.cursor > 0 {
                                tr.length_frames = tr.cursor;
                                tr.cursor = 0;
                                tr.layer_count = 1; // layer 0 finalised
                                tr.set_state(TrackState::Playing);
                                self.pending_quantize_stop[t] = None;
                            }
                        }
                    }
                    TrackState::Playing => {
                        if tr.length_frames > 0 && !mute {
                            let mut l = 0.0_f32;
                            let mut r = 0.0_f32;
                            for k in 0..tr.layer_count {
                                l += tr.layers[k][tr.cursor * 2];
                                r += tr.layers[k][tr.cursor * 2 + 1];
                            }
                            wet_l += l * level;
                            wet_r += r * level;
                            tr.cursor = (tr.cursor + 1) % tr.length_frames;
                        }
                    }
                    TrackState::Overdubbing => {
                        // Read existing, mix new input on top (with feedback
                        // attenuating the old layer so loops can fade), write
                        // back, advance.
                        if tr.length_frames > 0 {
                            // Output: sum of the already-finalised layers.
                            let mut old_l = 0.0_f32;
                            let mut old_r = 0.0_f32;
                            for k in 0..tr.layer_count {
                                old_l += tr.layers[k][tr.cursor * 2];
                                old_r += tr.layers[k][tr.cursor * 2 + 1];
                            }
                            // Record the new input into the fresh overdub layer at
                            // `layer_count` (feedback attenuates its own prior content
                            // so repeated passes can fade). Guarded to a valid slot.
                            let od = tr.layer_count.min(MAX_LAYERS - 1);
                            let prev_l = tr.layers[od][tr.cursor * 2];
                            let prev_r = tr.layers[od][tr.cursor * 2 + 1];
                            tr.layers[od][tr.cursor * 2] = (prev_l * fb + dry_l).clamp(-1.5, 1.5);
                            tr.layers[od][tr.cursor * 2 + 1] = (prev_r * fb + dry_r).clamp(-1.5, 1.5);
                            if !mute {
                                wet_l += old_l * level;
                                wet_r += old_r * level;
                            }
                            tr.cursor = (tr.cursor + 1) % tr.length_frames;
                        }
                    }
                    _ => {}
                }
            }

            let out_l = dry_l * dry_gain + wet_l;
            let out_r = dry_r * dry_gain + wet_r;
            l_write[i] = out_l * master_lin;
            r_write[i] = out_r * master_lin;
            self.shared.scope.push((out_l + out_r) * 0.5 * master_lin);
        }
    }

    fn render_mono(
        &mut self,
        l_read: &[f32], l_write: &mut [f32],
        dry_gain: f32, master_lin: f32,
        levels: &[f32; 4], feedbacks: &[f32; 4], mutes: &[bool; 4],
        sync_on: bool, bars_target: u32, bar_samples: usize,
    ) {
        // Reuse the stereo path with a mirrored channel. Simpler than a
        // second fully-tested implementation — but the mirror buffers are
        // pre-allocated, not `vec!`'d per block.
        let n = l_read.len().min(l_write.len()).min(self.mono_in.len());
        // Move the scratch out so render_stereo can borrow &mut self, then put
        // BOTH buffers back — leaving either taken would hand the next block an
        // empty slice and panic on the first index.
        let mut mono_in = std::mem::take(&mut self.mono_in);
        let mut mono_out = std::mem::take(&mut self.mono_out);
        mono_in[..n].copy_from_slice(&l_read[..n]);
        self.render_stereo(
            &l_read[..n], &mut l_write[..n], &mono_in[..n], &mut mono_out[..n],
            dry_gain, master_lin, levels, feedbacks, mutes,
            sync_on, bars_target, bar_samples,
        );
        self.mono_in = mono_in;
        self.mono_out = mono_out;
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 { 1 }
    fn get(&mut self, index: u32, is_input: bool, w: &mut AudioPortInfoWriter) {
        if index != 0 { return; }
        w.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: if is_input { b"Input" } else { b"Output" },
            channel_count: 2,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            in_place_pair: Some(ClapId::new(0)),
        });
    }
}

impl PluginNotePortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 { if is_input { 1 } else { 0 } }
    fn get(&mut self, index: u32, is_input: bool, w: &mut NotePortInfoWriter) {
        if !is_input || index != 0 { return; }
        w.set(&NotePortInfo {
            id: ClapId::new(0), name: b"MIDI Control",
            supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
            preferred_dialect: Some(NoteDialect::Midi),
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
        use core::fmt::Write;
        let pid = id.get() as usize;
        if pid == P_SYNC { return write!(w, "{}", if v >= 0.5 { "On" } else { "Off" }); }
        if pid == P_BARS {
            let b = v.round() as u32;
            return if b == 0 { write!(w, "Auto") } else { write!(w, "{} bars", b) };
        }
        // Per-track mute slots are at offset 2 within each track block.
        if pid >= GLOBAL_PARAM_COUNT && (pid - GLOBAL_PARAM_COUNT) % PARAMS_PER_TRACK == 2 {
            return write!(w, "{}", if v >= 0.5 { "Muted" } else { "On" });
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

pub struct SuperDuperLooper;

impl Plugin for SuperDuperLooper {
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

impl DefaultPluginFactory for SuperDuperLooper {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("co.superduperai.looper", plugin_display_name!("SuperDuper Looper"))
            .with_vendor("SuperDuperAI")
            .with_version(version_string!("0.2"))
            .with_description("Live looper — 4 tracks, host-BPM sync, MIDI CC remote, Mobius-style state")
            .with_features([AUDIO_EFFECT, STEREO])
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        Ok(PluginShared::new())
    }
    fn new_main_thread<'a>(_host: HostMainThreadHandle<'a>, shared: &'a PluginShared)
        -> Result<PluginMainThread<'a>, PluginError>
    {
        Ok(PluginMainThread { shared, gui_handle: None, gui_resize: gui::new_resize_bridge() })
    }
}

clack_export_entry!(SinglePluginEntry<SuperDuperLooper>);

// Silence the unused-import warning for Match in builds without all
// the event branches active.
#[allow(dead_code)]
fn _silence_match(_: Match<u16>) {}
