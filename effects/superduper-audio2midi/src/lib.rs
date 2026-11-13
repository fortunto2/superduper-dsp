//! SuperDuper Audio2MIDI: audio in, notes out, for any pitched instrument. Guitar mode also
//! names the string and fret.
//!
//! While the transport plays, the audio thread pushes mono samples into a lock-free ring; a worker
//! thread turns them into takes stamped with song time (a jump or loop starts a new take).
//! Analyse (a momentary parameter, the GUI, or the end of a take with Auto on) runs Basic Pitch on
//! tract plus the chosen decoder on a thread of its own and hands the finished notes to the audio
//! thread through `pending` / `retired`, so nothing is allocated or freed there. Play replays the
//! notes as note events at their song positions; Thru is the level of the original audio, so it
//! can be turned down while the MIDI plays. Every analysis is written to
//! `~/Music/SuperDuper MIDI/<track>-<time>.mid` (+ `.json`).

#![allow(clippy::missing_safety_doc)]

pub mod capture;
pub mod export;
pub mod gui;
pub mod mail;
pub mod notes;
pub mod player;
pub mod worker;

use atomic_float::AtomicF32;
use clack_common::stream::{InputStream, OutputStream};
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts, PluginAudioPortsImpl,
};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts, PluginNotePortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::events::event_types::TransportFlags;
use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use parking_lot::Mutex;
use std::ffi::CStr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{plugin_display_name, version_string};

pub use notes::{Ev, NoteSet, Record};
pub use superduper_transcribe::{Mode, Note, Settings};

fn init_logging() {
    superduper_dsp_sdk::log::init("audio2midi");
}

// Param table: frozen once shipped (REAPER caches it per slot). Append only.
pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Mode", min: 0.0, max: 1.0, default: 0.0, unit: "" }, // 0 Any, 1 Guitar
    ParamDef { id: 1, name: b"Capo", min: 0.0, max: 12.0, default: 0.0, unit: "" },
    ParamDef { id: 2, name: b"Max Fret", min: 3.0, max: 24.0, default: 19.0, unit: "" },
    // Momentary: 0 -> 1 starts an analysis, then the plugin sets it back to 0.
    ParamDef { id: 3, name: b"Analyse", min: 0.0, max: 1.0, default: 0.0, unit: "" },
    // Analyse by itself when the transport stops.
    ParamDef { id: 4, name: b"Auto", min: 0.0, max: 1.0, default: 1.0, unit: "" },
    ParamDef { id: 5, name: b"Play", min: 0.0, max: 1.0, default: 1.0, unit: "" },
    // Level of the original audio at the output (0 = only the MIDI plays).
    ParamDef { id: 6, name: b"Thru", min: 0.0, max: 1.0, default: 1.0, unit: "" },
    ParamDef { id: 7, name: b"Onset", min: 0.1, max: 0.9, default: 0.5, unit: "" },
    ParamDef { id: 8, name: b"Frame", min: 0.1, max: 0.7, default: 0.3, unit: "" },
    // Record while the transport plays.
    ParamDef { id: 9, name: b"Capture", min: 0.0, max: 1.0, default: 1.0, unit: "" },
];
const STEPPED_PARAMS: &[u32] = &[0, 1, 2, 3, 4, 5, 9];

pub const P_MODE: usize = 0;
pub const P_CAPO: usize = 1;
pub const P_MAX_FRET: usize = 2;
pub const P_ANALYSE: usize = 3;
pub const P_AUTO: usize = 4;
pub const P_PLAY: usize = 5;
pub const P_THRU: usize = 6;
pub const P_ONSET_THR: usize = 7;
pub const P_FRAME_THR: usize = 8;
pub const P_CAPTURE: usize = 9;

pub type SharedParams = Arc<Inner>;

const RETIRE_CAPACITY: usize = 8;

pub struct Inner {
    pub params: [AtomicF32; PARAMS.len()],
    pub dirty_params: [AtomicBool; PARAMS.len()],
    pub gesture_begin: [AtomicBool; PARAMS.len()],
    pub gesture_end: [AtomicBool; PARAMS.len()],
    pub bypass: AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub host_sr: AtomicF32,
    pub out_dir: PathBuf,
    /// The capture ring's worker half, handed over by `activate`.
    pub rx_slot: Mutex<Option<capture::Rx>>,
    pub analyse_req: AtomicU32,
    /// Last Analyse level the worker (or a state load) has seen; a rise from here starts an analysis.
    pub analyse_seen_high: AtomicBool,
    pub clear_req: AtomicU32,
    pub capturing: AtomicBool,
    pub captured_secs: AtomicF32,
    pub takes_n: AtomicU32,
    pub analysing: AtomicBool,
    pub rerun: AtomicBool,
    pub progress: AtomicF32,
    /// Song position while the transport plays, −1 otherwise (GUI playhead).
    pub play_pos: AtomicF32,
    pub status: Mutex<String>,
    pub record: Mutex<Option<Arc<Record>>>,
    pub note_count: AtomicU32,
    pub pending: Mutex<Option<Arc<NoteSet>>>,
    pub retired: Mutex<Vec<Arc<NoteSet>>>,
}

impl Inner {
    /// New transcription for the GUI, the state, the mailbox and the audio thread.
    pub fn publish(&self, rec: Option<Arc<Record>>, set: Arc<NoteSet>) {
        self.note_count.store(rec.as_ref().map_or(0, |r| r.notes.len() as u32), Ordering::Relaxed);
        *self.record.lock() = rec;
        let stale = self.pending.lock().replace(set);
        drop(stale);
        self.retired.lock().clear();
    }
}

superduper_synth_core::impl_shared_host!(Inner);

pub struct PluginShared {
    pub inner: SharedParams,
}

impl PluginShared {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                dirty_params: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| AtomicBool::new(false)),
                bypass: AtomicBool::new(false),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(4096),
                host_sr: AtomicF32::new(48_000.0),
                out_dir: export::default_out_dir(),
                rx_slot: Mutex::new(None),
                analyse_req: AtomicU32::new(0),
                analyse_seen_high: AtomicBool::new(false),
                clear_req: AtomicU32::new(0),
                capturing: AtomicBool::new(false),
                captured_secs: AtomicF32::new(0.0),
                takes_n: AtomicU32::new(0),
                analysing: AtomicBool::new(false),
                rerun: AtomicBool::new(false),
                progress: AtomicF32::new(0.0),
                play_pos: AtomicF32::new(-1.0),
                status: Mutex::new(String::new()),
                record: Mutex::new(None),
                note_count: AtomicU32::new(0),
                pending: Mutex::new(None),
                retired: Mutex::new(Vec::with_capacity(RETIRE_CAPACITY)),
            }),
        }
    }
    pub fn shared_handle(&self) -> SharedParams {
        Arc::clone(&self.inner)
    }
}

impl Default for PluginShared {
    fn default() -> Self {
        Self::new()
    }
}
impl std::ops::Deref for PluginShared {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.inner
    }
}
impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

/// GUI / agent: transcribe the audio captured so far.
pub fn request_analyse(s: &Inner) {
    s.analyse_req.fetch_add(1, Ordering::Release);
}

/// GUI: forget captured audio and notes.
pub fn request_clear(s: &Inner) {
    s.clear_req.fetch_add(1, Ordering::Release);
}

// ---------------------------------------------------------------------------
// Main thread + audio processor
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    track_watch: superduper_synth_core::gui::TrackInfoWatch<'a>,
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    tx: capture::Tx,
    player: player::Player,
    retire_slot: Option<Arc<NoteSet>>,
    sample_rate: f64,
    thru_gain: f32,
}

impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, PluginShared, PluginMainThread<'a>> for PluginAudioProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut PluginMainThread<'a>,
        shared: &'a PluginShared,
        cfg: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        init_logging();
        shared.scope.set_sample_rate(cfg.sample_rate as f32);
        shared.host_sr.store(cfg.sample_rate as f32, Ordering::Relaxed);
        let (tx, rx) = capture::channel(cfg.sample_rate as u32);
        *shared.rx_slot.lock() = Some(rx);
        Ok(Self {
            shared,
            tx,
            player: player::Player::new(),
            retire_slot: None,
            sample_rate: cfg.sample_rate,
            thru_gain: shared.params[P_THRU].load(Ordering::Relaxed),
        })
    }

    fn process(&mut self, process: Process, mut audio: Audio, mut events: Events) -> Result<ProcessStatus, PluginError> {
        let shared = self.shared;
        let _denormals = superduper_synth_core::effect::begin_events(&**shared, &process, &mut events);
        self.swap_in_pending(&mut events);

        let p = |i: usize| shared.params[i].load(Ordering::Relaxed);
        let transport = process.transport;
        let pos = transport
            .filter(|t| t.flags.contains(TransportFlags::IS_PLAYING | TransportFlags::HAS_SECONDS_TIMELINE))
            .map(|t| t.song_pos_seconds.to_float());
        let tempo = transport.filter(|t| t.flags.contains(TransportFlags::HAS_TEMPO)).map_or(120.0, |t| t.tempo);
        let capture_on = p(P_CAPTURE) >= 0.5;
        let thru_target = p(P_THRU).clamp(0.0, 1.0);
        let bypassed = shared.bypass.load(Ordering::Relaxed);

        let (tx, thru_gain) = (&mut self.tx, &mut self.thru_gain);
        let mut frames = 0usize;
        let mut edge = capture::Edge::None;
        let status = superduper_synth_core::effect::run_ports_with(&**shared, &mut audio, |bypassed, l_in, l_out, r| {
            frames = l_in.len();
            let target = if bypassed { 1.0 } else { thru_target };
            let step = (target - *thru_gain) / frames.max(1) as f32;
            let mut g = *thru_gain;
            for (o, &i) in l_out.iter_mut().zip(l_in) {
                g += step;
                *o = i * g;
            }
            let r_in: &[f32] = match r {
                Some((r_in, r_out)) => {
                    let mut g = *thru_gain;
                    for (o, &i) in r_out.iter_mut().zip(r_in) {
                        g += step;
                        *o = i * g;
                    }
                    r_in
                }
                None => l_in,
            };
            *thru_gain = target;
            edge = tx.block(pos.filter(|_| capture_on && !bypassed), tempo, l_in, r_in);
        });
        shared.capturing.store(self.tx.in_take(), Ordering::Relaxed);
        shared.play_pos.store(pos.map_or(-1.0, |v| v as f32), Ordering::Relaxed);
        if edge == capture::Edge::TakeClosed && p(P_AUTO) >= 0.5 {
            shared.analyse_req.fetch_add(1, Ordering::Release);
        }
        self.player.block(&mut events.output, pos, frames, self.sample_rate, p(P_PLAY) >= 0.5 && !bypassed);
        status
    }
}

impl PluginAudioProcessor<'_> {
    /// Take a finished note set from the worker; the old one goes back to be dropped there.
    fn swap_in_pending(&mut self, events: &mut Events) {
        if let Some(old) = self.retire_slot.take() {
            self.retire(old);
        }
        let fresh = self.shared.pending.try_lock().and_then(|mut g| g.take());
        if let Some(new) = fresh {
            let old = self.player.swap(new, &mut events.output);
            self.retire(old);
        }
    }

    fn retire(&mut self, old: Arc<NoteSet>) {
        match self.shared.retired.try_lock() {
            Some(mut list) if list.len() < list.capacity() => list.push(old),
            _ => self.retire_slot = Some(old),
        }
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 {
        1
    }
    fn get(&mut self, index: u32, is_input: bool, w: &mut AudioPortInfoWriter) {
        if index != 0 {
            return;
        }
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
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input { 0 } else { 1 }
    }
    fn get(&mut self, index: u32, is_input: bool, w: &mut NotePortInfoWriter) {
        if is_input || index != 0 {
            return;
        }
        w.set(&NotePortInfo {
            id: ClapId::new(0),
            name: b"MIDI Out",
            supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
            preferred_dialect: Some(NoteDialect::Clap),
        });
    }
}

impl PluginMainThread<'_> {
    fn display_text<W: core::fmt::Write>(&mut self, id: ClapId, v: f64, w: &mut W) -> core::fmt::Result {
        match id.get() as usize {
            P_MODE => return w.write_str(if v >= 0.5 { "Guitar" } else { "Any" }),
            P_ANALYSE | P_AUTO | P_PLAY | P_CAPTURE => return w.write_str(if v >= 0.5 { "On" } else { "Off" }),
            P_CAPO | P_MAX_FRET => return write!(w, "{}", v.round() as i32),
            _ => {}
        }
        ParamDef::write_display(PARAMS, id, v, w)
    }
}

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 {
        PARAMS.len() as u32
    }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info_stepped(PARAMS, idx, info, STEPPED_PARAMS);
    }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        self.shared.params.get(id.get() as usize).map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(&mut self, id: ClapId, v: f64, w: &mut ParamDisplayWriter) -> core::fmt::Result {
        self.display_text(id, v, w)
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        ParamDef::text_to_value_by_display(PARAMS, id, t, |v, s| self.display_text(id, v, s))
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

#[derive(serde::Serialize, serde::Deserialize)]
struct State {
    version: u32,
    params: Vec<f32>,
    bypass: bool,
    #[serde(default)]
    key_track: String,
    /// The last transcription, so a reloaded project keeps its notes.
    #[serde(default)]
    record: Option<Record>,
}

const STATE_VERSION: u32 = 1;

impl PluginStateImpl for PluginMainThread<'_> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        let s = &self.shared.inner;
        let state = State {
            version: STATE_VERSION,
            params: s.params.iter().map(|a| a.load(Ordering::Relaxed)).collect(),
            bypass: s.bypass.load(Ordering::Relaxed),
            key_track: s.scope.key_track.lock().clone(),
            record: s.record.lock().as_ref().map(|r| (**r).clone()),
        };
        serde_json::to_writer(output, &state).map_err(|_| PluginError::Message("state JSON error"))
    }

    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        let state: State = serde_json::from_reader(input).map_err(|_| PluginError::Message("state JSON error"))?;
        if state.version != STATE_VERSION {
            return Err(PluginError::Message("state version mismatch"));
        }
        let s = &self.shared.inner;
        for (i, v) in state.params.iter().enumerate() {
            if let Some(slot) = s.params.get(i) {
                slot.store(*v, Ordering::Relaxed);
            }
        }
        // a saved Analyse = 1 is restored as is, but must not fire on load
        s.analyse_seen_high.store(s.params[P_ANALYSE].load(Ordering::Relaxed) >= 0.5, Ordering::Relaxed);
        s.bypass.store(state.bypass, Ordering::Relaxed);
        *s.scope.key_track.lock() = state.key_track;
        let set = Arc::new(NoteSet::from_notes(state.record.as_ref().map_or(&[][..], |r| &r.notes)));
        s.publish(state.record.map(Arc::new), set);
        self.track_watch.rescan_param_values();
        Ok(())
    }
}

use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl, Window as ClapGuiWindow,
};

impl PluginGuiImpl for PluginMainThread<'_> {
    fn is_api_supported(&mut self, c: GuiConfiguration) -> bool {
        if c.is_floating {
            return false;
        }
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
    fn create(&mut self, _: GuiConfiguration) -> Result<(), PluginError> {
        Ok(())
    }
    fn destroy(&mut self) {
        self.gui_handle = None;
    }
    fn set_scale(&mut self, _: f64) -> Result<(), PluginError> {
        Ok(())
    }
    fn get_size(&mut self) -> Option<GuiSize> {
        Some(GuiSize { width: self.gui_resize.0.load(Ordering::Relaxed), height: self.gui_resize.1.load(Ordering::Relaxed) })
    }
    fn can_resize(&mut self) -> bool {
        true
    }
    fn get_resize_hints(&mut self) -> Option<GuiResizeHints> {
        Some(GuiResizeHints { can_resize_horizontally: true, can_resize_vertically: true, strategy: AspectRatioStrategy::Disregard })
    }
    fn adjust_size(&mut self, s: GuiSize) -> Option<GuiSize> {
        Some(GuiSize {
            width: s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH),
            height: s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT),
        })
    }
    fn set_size(&mut self, s: GuiSize) -> Result<(), PluginError> {
        self.gui_resize.0.store(s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH), Ordering::Relaxed);
        self.gui_resize.1.store(s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT), Ordering::Relaxed);
        Ok(())
    }
    fn set_parent(&mut self, window: ClapGuiWindow) -> Result<(), PluginError> {
        self.gui_handle = Some(gui::open_window(&window, self.shared.shared_handle(), self.gui_resize.clone()));
        Ok(())
    }
    fn show(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
    fn hide(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
    fn set_transient(&mut self, _: ClapGuiWindow) -> Result<(), PluginError> {
        Ok(())
    }
}

pub struct SuperDuperAudio2Midi;

impl Plugin for SuperDuperAudio2Midi {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::track_info::PluginTrackInfo>();
    }
}

impl DefaultPluginFactory for SuperDuperAudio2Midi {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new("co.superduperai.audio2midi", plugin_display_name!("SuperDuper Audio2MIDI"))
            .with_vendor("SuperDuperAI")
            .with_version(version_string!("0.1"))
            .with_description("Audio to MIDI for any instrument; Guitar mode names string and fret. Records while the transport plays, replays the notes as MIDI out")
            .with_features([AUDIO_EFFECT, ANALYZER, STEREO])
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        let shared = PluginShared::new();
        worker::spawn(&shared.inner);
        superduper_synth_core::gui::spawn_bus_publisher_mcp(
            "SuperDuper Audio2MIDI",
            &shared.inner,
            |s| &s.scope,
            Some(superduper_synth_core::gui::ParamsAccess { defs: PARAMS, values: |s| &s.params, dirty: Some(|s| &s.dirty_params) }),
            Some(mail::handle),
        );
        Ok(shared)
    }
    fn new_main_thread<'a>(host: HostMainThreadHandle<'a>, shared: &'a PluginShared) -> Result<PluginMainThread<'a>, PluginError> {
        let track_watch = superduper_synth_core::gui::TrackInfoWatch::new(host, &shared.scope);
        Ok(PluginMainThread { track_watch, shared, gui_handle: None, gui_resize: gui::new_resize_bridge() })
    }
}

clack_export_entry!(SinglePluginEntry<SuperDuperAudio2Midi>);

impl clack_extensions::track_info::PluginTrackInfoImpl for PluginMainThread<'_> {
    fn changed(&self) {
        self.track_watch.refresh();
    }
}
