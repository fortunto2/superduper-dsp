//! Audio thread -> worker: mono samples through a lock-free ring, take boundaries through a second
//! ring of markers stamped with the number of samples pushed so far. Nothing here allocates,
//! locks or does I/O on the audio side.

use rtrb::{Consumer, Producer, RingBuffer};

/// Samples the ring holds: ~5 s at 48 kHz. The worker drains every 20 ms.
pub const RING_SAMPLES: usize = 1 << 18;
const MARKER_SLOTS: usize = 256;
/// Longest take kept, seconds. A longer one stops growing (the status says so).
pub const MAX_TAKE_SECONDS: f64 = 600.0;
/// A song-position step bigger than this is a jump (loop, locate), not a block boundary.
const JUMP_SECONDS: f64 = 0.05;

#[derive(Debug, Clone, Copy)]
pub enum Marker {
    /// A take begins with the next sample, at song position `song_pos` seconds.
    Start { at: u64, song_pos: f64, sr: u32, tempo: f64 },
    Stop { at: u64 },
}

impl Marker {
    pub fn at(&self) -> u64 {
        match *self {
            Marker::Start { at, .. } | Marker::Stop { at } => at,
        }
    }
}

pub struct Tx {
    samples: Producer<f32>,
    markers: Producer<Marker>,
    pushed: u64,
    in_take: bool,
    expected: f64,
    sr: u32,
}

pub struct Rx {
    pub samples: Consumer<f32>,
    pub markers: Consumer<Marker>,
}

pub fn channel(sr: u32) -> (Tx, Rx) {
    let (sp, sc) = RingBuffer::new(RING_SAMPLES);
    let (mp, mc) = RingBuffer::new(MARKER_SLOTS);
    (
        Tx { samples: sp, markers: mp, pushed: 0, in_take: false, expected: 0.0, sr },
        Rx { samples: sc, markers: mc },
    )
}

/// What one block did to the capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    None,
    /// The transport stopped (or capture was switched off) and a take was closed.
    TakeClosed,
}

impl Tx {
    pub fn in_take(&self) -> bool {
        self.in_take
    }

    /// One block. `playing_pos` is the song position at the block start when the transport is
    /// playing with a seconds timeline, `None` otherwise (which closes any open take).
    pub fn block(&mut self, playing_pos: Option<f64>, tempo: f64, l: &[f32], r: &[f32]) -> Edge {
        let n = l.len().min(r.len());
        let Some(pos) = playing_pos.filter(|_| n > 0) else {
            return self.close();
        };
        let jumped = self.in_take && (pos - self.expected).abs() > JUMP_SECONDS;
        let mut edge = Edge::None;
        if jumped {
            edge = self.close();
        }
        if !self.in_take {
            let m = Marker::Start { at: self.pushed, song_pos: pos, sr: self.sr, tempo };
            if self.markers.push(m).is_err() {
                return edge;
            }
            self.in_take = true;
        }
        match self.samples.write_chunk_uninit(n) {
            Ok(chunk) => {
                chunk.fill_from_iter((0..n).map(|i| 0.5 * (l[i] + r[i])));
                self.pushed += n as u64;
                self.expected = pos + n as f64 / self.sr as f64;
            }
            Err(_) => {
                // The worker is behind: drop this block and restart the take after the gap, so
                // the timeline of what was kept stays honest.
                edge = self.close();
            }
        }
        edge
    }

    fn close(&mut self) -> Edge {
        if !self.in_take {
            return Edge::None;
        }
        self.in_take = false;
        let _ = self.markers.push(Marker::Stop { at: self.pushed });
        Edge::TakeClosed
    }
}

/// One contiguous stretch of audio and where it sat on the song timeline.
#[derive(Debug, Clone)]
pub struct Take {
    pub song_start: f64,
    pub sr: u32,
    pub tempo: f64,
    pub samples: Vec<f32>,
}

impl Take {
    pub fn seconds(&self) -> f64 {
        self.samples.len() as f64 / self.sr.max(1) as f64
    }
}

/// Worker side: drains the rings into takes.
#[derive(Default)]
pub struct Recorder {
    pub takes: Vec<Take>,
    open: bool,
    consumed: u64,
    /// Set after an analysis: the next take starts a fresh session and drops the old ones.
    pub fresh_session_pending: bool,
    pub truncated: bool,
}

impl Recorder {
    /// A new pair of rings starts counting from zero again.
    pub fn reset_counters(&mut self) {
        self.consumed = 0;
        self.open = false;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn drain(&mut self, rx: &mut Rx) {
        loop {
            let avail = rx.samples.slots();
            let next = rx.markers.peek().ok().copied();
            let limit = match next {
                Some(m) => m.at().saturating_sub(self.consumed) as usize,
                None => avail,
            };
            let n = limit.min(avail);
            if n > 0 {
                if let Ok(chunk) = rx.samples.read_chunk(n) {
                    let (a, b) = chunk.as_slices();
                    if let (true, Some(t)) = (self.open, self.takes.last_mut()) {
                        let room = ((MAX_TAKE_SECONDS * t.sr as f64) as usize).saturating_sub(t.samples.len());
                        if room < a.len() + b.len() {
                            self.truncated = true;
                        }
                        let take_a = a.len().min(room);
                        t.samples.extend_from_slice(&a[..take_a]);
                        t.samples.extend_from_slice(&b[..b.len().min(room - take_a)]);
                    }
                    chunk.commit_all();
                    self.consumed += n as u64;
                }
            }
            match next {
                Some(m) if self.consumed >= m.at() => {
                    let _ = rx.markers.pop();
                    self.apply(m);
                }
                _ if n == 0 => break,
                None => {}
                _ => {}
            }
        }
    }

    fn apply(&mut self, m: Marker) {
        match m {
            Marker::Start { song_pos, sr, tempo, .. } => {
                if self.fresh_session_pending {
                    self.takes.clear();
                    self.fresh_session_pending = false;
                    self.truncated = false;
                }
                self.takes.push(Take { song_start: song_pos, sr, tempo, samples: Vec::new() });
                self.open = true;
            }
            Marker::Stop { .. } => self.open = false,
        }
    }

    /// The take worth analysing: the longest, the latest of near-equal ones (a looped section
    /// recorded several times: the last full pass, not the partial one at the end).
    pub fn best_take(&self) -> Option<&Take> {
        let mut best: Option<&Take> = None;
        for t in self.takes.iter().filter(|t| t.seconds() >= 0.5) {
            if best.is_none_or(|b| t.seconds() >= b.seconds() * 0.98) {
                best = Some(t);
            }
        }
        best
    }

    pub fn total_seconds(&self) -> f64 {
        self.best_take().map_or(0.0, |t| t.seconds())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(tx: &mut Tx, rx: &mut Rx, rec: &mut Recorder, pos: Option<f64>, n: usize, v: f32) -> Edge {
        let l = vec![v; n];
        let e = tx.block(pos, 120.0, &l, &l);
        rec.drain(rx);
        e
    }

    #[test]
    fn contiguous_blocks_make_one_take() {
        let (mut tx, mut rx) = channel(48000);
        let mut rec = Recorder::default();
        for i in 0..10 {
            run(&mut tx, &mut rx, &mut rec, Some(5.0 + i as f64 * 0.01), 480, 0.5);
        }
        assert_eq!(run(&mut tx, &mut rx, &mut rec, None, 480, 0.0), Edge::TakeClosed);
        assert_eq!(rec.takes.len(), 1);
        assert_eq!(rec.takes[0].samples.len(), 4800);
        assert!((rec.takes[0].song_start - 5.0).abs() < 1e-9);
        assert!(!rec.is_open());
    }

    #[test]
    fn a_jump_starts_a_new_take_at_the_new_position() {
        let (mut tx, mut rx) = channel(48000);
        let mut rec = Recorder::default();
        for i in 0..5 {
            run(&mut tx, &mut rx, &mut rec, Some(1.0 + i as f64 * 0.01), 480, 1.0);
        }
        // loop back to 0
        for i in 0..3 {
            run(&mut tx, &mut rx, &mut rec, Some(i as f64 * 0.01), 480, 2.0);
        }
        assert_eq!(rec.takes.len(), 2);
        assert_eq!(rec.takes[0].samples.len(), 2400);
        assert_eq!(rec.takes[1].samples.len(), 1440);
        assert!(rec.takes[0].samples.iter().all(|&s| s == 1.0) && rec.takes[1].samples.iter().all(|&s| s == 2.0));
    }

    #[test]
    fn a_full_ring_drops_the_block_and_restarts_cleanly() {
        let (mut tx, mut rx) = channel(48000);
        let mut rec = Recorder::default();
        let big = vec![0.1f32; 100_000];
        let mut pos = 0.0;
        for _ in 0..4 {
            tx.block(Some(pos), 120.0, &big, &big);
            pos += 100_000.0 / 48000.0;
        }
        rec.drain(&mut rx);
        let kept: usize = rec.takes.iter().map(|t| t.samples.len()).sum();
        assert!(kept > 0 && kept < 400_000, "kept {kept}");
        assert!(rec.takes.len() >= 2, "a gap must split the take");
    }

    #[test]
    fn best_take_prefers_the_last_full_pass() {
        let mut rec = Recorder::default();
        let mk = |s: f64, secs: f64| Take { song_start: s, sr: 1000, tempo: 120.0, samples: vec![0.0; (secs * 1000.0) as usize] };
        rec.takes = vec![mk(0.0, 8.0), mk(0.0, 8.0), mk(0.0, 3.0)];
        let best = rec.best_take().unwrap();
        assert!((best.seconds() - 8.0).abs() < 1e-9);
        assert!(std::ptr::eq(best, &rec.takes[1]));
    }
}
