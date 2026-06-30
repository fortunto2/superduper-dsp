//! One loop-track — owns a STACK OF LAYER BUFFERS + its state machine.
//! RT-safe: every layer buffer is pre-allocated at activate(), no heap is
//! touched in process().
//!
//! Unlike a classic single-buffer looper, each overdub pass records into its
//! OWN buffer (a "layer"), so layers can be seen, undone, and volume-mixed
//! independently — the whole point of this looper.
//!
//! State machine (Mobius-style, so muscle memory transfers):
//!
//! ```text
//!   Empty ── Rec ─► Recording ── Rec ─► Playing   (layer 0 locked)
//!   Playing ── Overdub ─► Overdubbing ── Overdub ─► Playing  (+1 layer)
//!   Any ── Stop ─► Stopped       Any ── Clear ─► Empty (all layers wiped)
//!   Any ── Undo ─► drop the last layer (cancels an in-progress overdub)
//! ```

use std::sync::atomic::{AtomicU32, Ordering};

/// Max overdub layers per track. Each is a pre-allocated full-length buffer,
/// so this × MAX_LOOP_SECONDS × TRACK_COUNT bounds the looper's RAM.
pub const MAX_LAYERS: usize = 6;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TrackState {
    Empty = 0,
    Recording = 1,
    Playing = 2,
    Overdubbing = 3,
    Stopped = 4,
}

impl TrackState {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => TrackState::Recording,
            2 => TrackState::Playing,
            3 => TrackState::Overdubbing,
            4 => TrackState::Stopped,
            _ => TrackState::Empty,
        }
    }
}

/// Pending button event submitted by the GUI / MIDI handlers. The audio
/// thread consumes one per process() call.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TrackCommand {
    None = 0,
    Rec = 1,
    PlayStop = 2,
    Overdub = 3,
    Clear = 4,
    Undo = 5,
}

impl TrackCommand {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => TrackCommand::Rec,
            2 => TrackCommand::PlayStop,
            3 => TrackCommand::Overdub,
            4 => TrackCommand::Clear,
            5 => TrackCommand::Undo,
            _ => TrackCommand::None,
        }
    }
}

/// Per-track persistent state. `layers[i]` is interleaved L/R (L,R,L,R…),
/// each `capacity_frames × 2` long. `layer_count` is how many are FINALISED;
/// while overdubbing, the in-progress layer is `layers[layer_count]`.
pub struct LoopTrack {
    pub layers: Vec<Vec<f32>>,
    /// Finalised (audible, undoable) layer count.
    pub layer_count: usize,
    /// Locked loop length in frames (set when recording stops).
    pub length_frames: usize,
    /// Playback / record cursor in frames.
    pub cursor: usize,
    /// True while a fresh layer is being recorded under Overdubbing.
    pub overdub_active: bool,
    pub state_atom: AtomicU32,
    pub command_atom: AtomicU32,
}

impl LoopTrack {
    pub fn new(capacity_frames: usize) -> Self {
        Self {
            layers: (0..MAX_LAYERS).map(|_| vec![0.0; capacity_frames * 2]).collect(),
            layer_count: 0,
            length_frames: 0,
            cursor: 0,
            overdub_active: false,
            state_atom: AtomicU32::new(TrackState::Empty as u32),
            command_atom: AtomicU32::new(TrackCommand::None as u32),
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize { self.layers[0].len() / 2 }

    #[inline]
    pub fn state(&self) -> TrackState {
        TrackState::from_u32(self.state_atom.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn submit(&self, cmd: TrackCommand) {
        self.command_atom.store(cmd as u32, Ordering::Release);
    }

    #[inline]
    pub fn take_command(&self) -> TrackCommand {
        TrackCommand::from_u32(self.command_atom.swap(0, Ordering::AcqRel))
    }

    #[inline]
    pub fn set_state(&mut self, s: TrackState) {
        self.state_atom.store(s as u32, Ordering::Release);
    }

    /// The index of the layer currently being recorded into (layer 0 for the
    /// first record, `layer_count` during an overdub), if any.
    #[inline]
    pub fn recording_layer(&self) -> Option<usize> {
        match self.state() {
            TrackState::Recording => Some(0),
            TrackState::Overdubbing if self.overdub_active => Some(self.layer_count),
            _ => None,
        }
    }

    /// Wipe one layer's buffer to silence.
    #[inline]
    fn wipe(&mut self, layer: usize) {
        if let Some(buf) = self.layers.get_mut(layer) {
            for s in buf.iter_mut() { *s = 0.0; }
        }
    }

    /// Reset everything to Empty (all layers wiped).
    pub fn clear(&mut self) {
        for i in 0..self.layers.len() { self.wipe(i); }
        self.layer_count = 0;
        self.length_frames = 0;
        self.cursor = 0;
        self.overdub_active = false;
        self.set_state(TrackState::Empty);
    }

    /// Start a fresh recording into layer 0.
    pub fn begin_record(&mut self) {
        self.clear();
        self.cursor = 0;
        self.set_state(TrackState::Recording);
    }

    /// Begin an overdub pass: a new layer at `layer_count` (if room).
    /// Returns false (and stays Playing) when the layer stack is full.
    pub fn begin_overdub(&mut self) -> bool {
        if self.layer_count >= MAX_LAYERS {
            return false;
        }
        self.wipe(self.layer_count);   // fresh empty layer to record into
        self.overdub_active = true;
        self.set_state(TrackState::Overdubbing);
        true
    }

    /// Finalise the in-progress overdub layer into the mix.
    pub fn finalize_overdub(&mut self) {
        if self.overdub_active && self.layer_count < MAX_LAYERS {
            self.layer_count += 1;
        }
        self.overdub_active = false;
        self.set_state(TrackState::Playing);
    }

    /// Drop the most recent layer. Cancels an in-progress overdub first;
    /// otherwise pops the last finalised layer. Becomes Empty at zero layers.
    pub fn undo(&mut self) {
        if self.overdub_active {
            self.wipe(self.layer_count);
            self.overdub_active = false;
            self.set_state(TrackState::Playing);
            return;
        }
        if self.layer_count > 0 {
            self.layer_count -= 1;
            self.wipe(self.layer_count);
        }
        if self.layer_count == 0 {
            self.length_frames = 0;
            self.cursor = 0;
            self.set_state(TrackState::Empty);
        }
    }
}
