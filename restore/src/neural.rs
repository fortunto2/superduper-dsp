//! Neural noise suppression: RNNoise (Valin 2018) through the pure-Rust `nnnoiseless` crate,
//! model compiled in, no ONNX, no model file. BSD-3-Clause.
//!
//! RNNoise works on 480-sample frames at 48 kHz and nowhere else. Streaming: samples collect
//! into a frame (480 of delay), the network returns the previous frame (480 more), and a pad
//! delay brings the total to the crate's common latency so switching modes moves nothing.
//! Measured on Apple silicon: ~17 µs per 10 ms frame per channel.

use crate::Delay;
use nnnoiseless::DenoiseState;

const FRAME: usize = DenoiseState::FRAME_SIZE;
/// Frame collection + the network's own one-frame delay (measured with a chirp: 480).
const OWN_LATENCY: usize = 2 * FRAME;
/// nnnoiseless expects 16-bit PCM magnitudes in f32.
const PCM: f32 = 32768.0;
pub(crate) const RATE: f32 = 48_000.0;

pub(crate) struct Neural {
    states: Vec<Box<DenoiseState<'static>>>,
    input: Vec<[f32; FRAME]>,
    output: Vec<[f32; FRAME]>,
    pos: usize,
    pad: Delay,
}

impl Neural {
    /// `latency` is the common one to match; it must be at least two frames.
    pub(crate) fn new(channels: usize, latency: usize) -> Self {
        assert!(latency >= OWN_LATENCY, "common latency {latency} below RNNoise's {OWN_LATENCY}");
        Neural {
            states: (0..channels).map(|_| DenoiseState::new()).collect(),
            input: vec![[0.0; FRAME]; channels],
            output: vec![[0.0; FRAME]; channels],
            pos: 0,
            pad: Delay::new(latency - OWN_LATENCY, channels),
        }
    }

    pub(crate) fn reset(&mut self) {
        self.states.iter_mut().for_each(|s| *s = DenoiseState::new());
        self.input.iter_mut().for_each(|f| *f = [0.0; FRAME]);
        self.output.iter_mut().for_each(|f| *f = [0.0; FRAME]);
        self.pos = 0;
        self.pad.reset();
    }

    pub(crate) fn process(&mut self, channels: &mut [&mut [f32]]) {
        let nch = channels.len().min(self.states.len());
        let len = channels.iter().take(nch).map(|c| c.len()).min().unwrap_or(0);
        // Frames are shared across channels, so walk sample by sample in lockstep.
        for i in 0..len {
            for (ch, buf) in channels.iter_mut().enumerate().take(nch) {
                let x = buf[i];
                buf[i] = self.output[ch][self.pos] / PCM;
                self.input[ch][self.pos] = x * PCM;
            }
            self.pos += 1;
            if self.pos == FRAME {
                self.pos = 0;
                for ch in 0..nch {
                    self.states[ch].process_frame(&mut self.output[ch], &self.input[ch]);
                }
            }
        }
        self.pad.process(&mut channels[..nch]);
    }
}
