//! Replays the transcription as note events at their song positions. Audio thread only: no
//! allocation, no locks. A jump, a stop, a new note set or Play switched off ends every note
//! that is sounding.

use crate::notes::NoteSet;
use clack_common::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_common::events::Pckn;
use clack_plugin::prelude::OutputEvents;
use std::sync::Arc;

const JUMP_SECONDS: f64 = 0.05;

pub struct Player {
    pub set: Arc<NoteSet>,
    /// Per key: bit `c` set while channel `c` has a note on.
    sounding: [u16; 128],
    expected: f64,
    running: bool,
}

impl Player {
    pub fn new() -> Self {
        Self { set: Arc::new(NoteSet::default()), sounding: [0; 128], expected: 0.0, running: false }
    }

    pub fn any_sounding(&self) -> bool {
        self.sounding.iter().any(|&b| b != 0)
    }

    fn off(&mut self, out: &mut OutputEvents, time: u32, key: u8, ch: u8) {
        self.sounding[key as usize] &= !(1 << ch);
        let _ = out.try_push(&NoteOffEvent::new(time, Pckn::new(0u16, ch as u16, key as u16, 0u32), 0.0));
    }

    pub fn all_off(&mut self, out: &mut OutputEvents, time: u32) {
        for key in 0..128u8 {
            let bits = self.sounding[key as usize];
            if bits == 0 {
                continue;
            }
            for ch in 0..16u8 {
                if bits & (1 << ch) != 0 {
                    self.off(out, time, key, ch);
                }
            }
        }
    }

    /// Replace the note set; sounding notes end first.
    pub fn swap(&mut self, new: Arc<NoteSet>, out: &mut OutputEvents) -> Arc<NoteSet> {
        self.all_off(out, 0);
        self.running = false;
        std::mem::replace(&mut self.set, new)
    }

    /// One block of `frames` at `sr`. `pos` is the song position at the block start while the
    /// transport plays, `None` otherwise.
    pub fn block(&mut self, out: &mut OutputEvents, pos: Option<f64>, frames: usize, sr: f64, enabled: bool) {
        let Some(p0) = pos.filter(|_| enabled && frames > 0) else {
            if self.any_sounding() {
                self.all_off(out, 0);
            }
            self.running = false;
            return;
        };
        let p1 = p0 + frames as f64 / sr;
        if !self.running || (p0 - self.expected).abs() > JUMP_SECONDS {
            self.all_off(out, 0);
            self.running = true;
        }
        self.expected = p1;
        let set = Arc::clone(&self.set);
        let ev = &set.ev;
        let lo = ev.partition_point(|e| e.t < p0);
        // events are copied out (Copy) so `self` stays free for the note bookkeeping
        let mut i = lo;
        while i < ev.len() && ev[i].t < p1 {
            let e = ev[i];
            let time = (((e.t - p0) * sr) as i64).clamp(0, frames as i64 - 1) as u32;
            let bit = 1u16 << e.ch.min(15);
            if e.vel == 0 {
                // an off for a note that never started here (we jumped into it) is dropped
                if self.sounding[e.key as usize] & bit != 0 {
                    self.off(out, time, e.key, e.ch);
                }
            } else {
                if self.sounding[e.key as usize] & bit != 0 {
                    self.off(out, time, e.key, e.ch);
                }
                self.sounding[e.key as usize] |= bit;
                let _ = out.try_push(&NoteOnEvent::new(
                    time,
                    Pckn::new(0u16, e.ch as u16, e.key as u16, 0u32),
                    e.vel as f64 / 127.0,
                ));
            }
            i += 1;
        }
    }
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}
