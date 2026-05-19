//! One loop-track — owns its audio buffer + state machine. RT-safe:
//! buffer is pre-allocated at activate(), no heap touched in process().
//!
//! State machine (matches Mobius semantics so muscle memory transfers):
//!
//! ```text
//!     Empty
//!       │   Rec pressed
//!       ▼
//!   Recording  ─── Rec pressed again ──► Playing (loop length locked)
//!       │
//!       │  (also auto-stops at bar-aligned length when sync is on)
//!
//!   Playing  ──── Overdub pressed ──► Overdubbing
//!      ▲                                  │
//!      └───── Overdub pressed again ──────┘
//!
//!   Any state ── Stop pressed ──► Stopped (buffer kept)
//!   Any state ── Clear pressed ──► Empty   (buffer wiped to zero)
//! ```

use std::sync::atomic::{AtomicU32, Ordering};

/// Track state — one slot per UI button. Stored as an atomic u32 so
/// the audio thread can read it lock-free and the GUI can poll it
/// for indicator colour.
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

/// Pending button event submitted by the GUI / MIDI input handlers.
/// Audio thread consumes one per process() call and updates the
/// state machine accordingly.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TrackCommand {
    None = 0,
    Rec = 1,
    PlayStop = 2,
    Overdub = 3,
    Clear = 4,
}

impl TrackCommand {
    pub fn from_u32(v: u32) -> Self {
        match v {
            1 => TrackCommand::Rec,
            2 => TrackCommand::PlayStop,
            3 => TrackCommand::Overdub,
            4 => TrackCommand::Clear,
            _ => TrackCommand::None,
        }
    }
}

/// Per-track persistent state. The buffer is two interleaved f32s
/// per frame (L, R, L, R, …) so a single Vec backs the whole loop.
pub struct LoopTrack {
    /// Interleaved L/R audio. Length = capacity_frames × 2.
    pub buffer: Vec<f32>,
    /// Number of frames recorded so far (or the locked loop length
    /// once recording is over). Capped at capacity_frames.
    pub length_frames: usize,
    /// Playback / record cursor in frames.
    pub cursor: usize,
    /// Read once per block by the audio thread.
    pub state_atom: AtomicU32,
    /// GUI / MIDI submitted commands. Audio thread takes them with
    /// `swap(0)` so each press fires exactly once.
    pub command_atom: AtomicU32,
}

impl LoopTrack {
    /// Build an empty track with a fixed maximum length in frames.
    pub fn new(capacity_frames: usize) -> Self {
        Self {
            buffer: vec![0.0; capacity_frames * 2],
            length_frames: 0,
            cursor: 0,
            state_atom: AtomicU32::new(TrackState::Empty as u32),
            command_atom: AtomicU32::new(TrackCommand::None as u32),
        }
    }

    /// Maximum length in frames (capacity).
    #[inline]
    pub fn capacity(&self) -> usize { self.buffer.len() / 2 }

    /// Read the current state.
    #[inline]
    pub fn state(&self) -> TrackState {
        TrackState::from_u32(self.state_atom.load(Ordering::Relaxed))
    }

    /// Submit a command (called from GUI / MIDI handler thread).
    #[inline]
    pub fn submit(&self, cmd: TrackCommand) {
        self.command_atom.store(cmd as u32, Ordering::Release);
    }

    /// Take the pending command (audio-thread only).
    #[inline]
    pub fn take_command(&self) -> TrackCommand {
        TrackCommand::from_u32(self.command_atom.swap(0, Ordering::AcqRel))
    }

    /// Set the state both internally and visible to the GUI atom.
    #[inline]
    pub fn set_state(&mut self, s: TrackState) {
        self.state_atom.store(s as u32, Ordering::Release);
    }

    /// Wipe the buffer and reset cursor / length / state.
    pub fn clear(&mut self) {
        for s in self.buffer.iter_mut() { *s = 0.0; }
        self.length_frames = 0;
        self.cursor = 0;
        self.set_state(TrackState::Empty);
    }
}
