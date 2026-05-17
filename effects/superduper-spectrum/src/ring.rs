//! Helpers for the GUI side of the audio→GUI ring buffer.
//!
//! The audio thread pushes individual samples into an `rtrb::Producer<f32>`
//! every block. The GUI thread, once per frame, drains as much as it can
//! and folds the new samples into a sliding history buffer that gets fed
//! straight into the FFT analyzer.

use rtrb::Consumer;

/// Sliding mono history buffer used as the FFT input.
pub struct SlidingHistory {
    pub buf: Vec<f32>,
    pub write: usize,
}

impl SlidingHistory {
    pub fn new(size: usize) -> Self {
        Self {
            buf: vec![0.0; size.max(1024)],
            write: 0,
        }
    }

    /// Resize without losing data fits when the user changes FFT size.
    /// Drops stale samples — the GUI immediately fills the new size from
    /// the audio stream.
    pub fn resize(&mut self, new_len: usize) {
        if new_len == self.buf.len() {
            return;
        }
        self.buf = vec![0.0; new_len.max(1024)];
        self.write = 0;
    }

    /// Drain the ring buffer into the history (overwriting oldest samples).
    pub fn drain_from(&mut self, consumer: &mut Consumer<f32>) {
        while let Ok(s) = consumer.pop() {
            self.buf[self.write] = s;
            self.write = (self.write + 1) % self.buf.len();
        }
    }

    /// Copy the history into a linear buffer for FFT (oldest sample first).
    pub fn linear(&self, out: &mut Vec<f32>) {
        out.clear();
        out.reserve(self.buf.len());
        out.extend_from_slice(&self.buf[self.write..]);
        out.extend_from_slice(&self.buf[..self.write]);
    }
}
