//! Basic Pitch inference: Spotify's ICASSP 2022 model (`nmp.onnx`, Apache-2.0, embedded) on
//! tract. Windowing, overlap and unwrapping replicate `basic_pitch/inference.py`.

use std::sync::Arc;
use tract_onnx::prelude::*;

pub const SAMPLE_RATE: u32 = 22_050;
pub const FFT_HOP: usize = 256;
/// Samples per model window (2 s minus one hop).
pub const WINDOW: usize = SAMPLE_RATE as usize * 2 - FFT_HOP; // 43844
/// Frames the model returns per window.
pub const WINDOW_FRAMES: usize = 172;
/// Frames of overlap between windows (Basic Pitch default).
pub const OVERLAP_FRAMES: usize = 30;
/// Pitches: piano keys, MIDI 21..=108.
pub const PITCHES: usize = 88;
pub const MIDI_OFFSET: usize = 21;

static MODEL_BYTES: &[u8] = include_bytes!("../model/nmp.onnx");

// The Python wrapper names the heads by these ONNX outputs (basic_pitch/inference.py).
const OUT_NOTE: &str = "StatefulPartitionedCall:1";
const OUT_ONSET: &str = "StatefulPartitionedCall:2";

type Plan = TypedRunnableModel<TypedModel>;

/// Per-frame note and onset probabilities, `frames x 88`, row-major.
pub struct Posteriors {
    pub frames: usize,
    pub note: Vec<f32>,
    pub onset: Vec<f32>,
}

impl Posteriors {
    pub fn note_at(&self, frame: usize, pitch: usize) -> f32 {
        self.note[frame * PITCHES + pitch]
    }
    pub fn onset_at(&self, frame: usize, pitch: usize) -> f32 {
        self.onset[frame * PITCHES + pitch]
    }
    pub fn note_col(&self, pitch: usize) -> impl Iterator<Item = f32> + '_ {
        (0..self.frames).map(move |f| self.note_at(f, pitch))
    }
}

/// Basic Pitch's frame -> seconds, including its per-window drift correction (each 2 s window
/// starts 10.24 ms early, so `frame * 256 / 22050` alone drifts 0.3 s per minute).
pub fn frame_time(f: usize) -> f64 {
    const WIN_OFF: f64 =
        (FFT_HOP as f64 / SAMPLE_RATE as f64) * (WINDOW_FRAMES as f64 - WINDOW as f64 / FFT_HOP as f64) + 0.0018;
    f as f64 * FFT_HOP as f64 / SAMPLE_RATE as f64 - WIN_OFF * (f / WINDOW_FRAMES) as f64
}

pub struct Model {
    plan: Arc<Plan>,
}

impl Model {
    /// Parse and optimise the embedded model (~100 ms).
    pub fn load() -> TractResult<Self> {
        let plan = tract_onnx::onnx()
            .model_for_read(&mut &MODEL_BYTES[..])?
            .with_input_fact(0, f32::fact([1, WINDOW, 1]).into())?
            // the contour head (pitch bends) is not used: leave it out of the graph
            .with_output_names([OUT_NOTE, OUT_ONSET])?
            .into_optimized()?
            .into_runnable()?;
        Ok(Self { plan: plan.into() })
    }

    fn run_window(&self, window: &[f32]) -> TractResult<(Vec<f32>, Vec<f32>)> {
        let input: Tensor = tract_ndarray::Array3::from_shape_vec((1, WINDOW, 1), window.to_vec())?.into();
        let out = self.plan.run(tvec!(input.into()))?;
        let note = out[0].to_array_view::<f32>()?.iter().copied().collect();
        let onset = out[1].to_array_view::<f32>()?.iter().copied().collect();
        Ok((note, onset))
    }

    /// Posteriors for mono audio at 22050 Hz. `progress` gets 0..1 after every window and may
    /// return `false` to cancel (the result is then `None`).
    pub fn posteriors(&self, audio: &[f32], mut progress: impl FnMut(f32) -> bool) -> TractResult<Option<Posteriors>> {
        let overlap = OVERLAP_FRAMES * FFT_HOP;
        let hop = WINDOW - overlap;
        let trim = OVERLAP_FRAMES / 2;
        // the signal is preceded by half an overlap of silence
        let mut padded = vec![0.0f32; overlap / 2];
        padded.extend_from_slice(audio);

        let n_windows = padded.len().div_ceil(hop);
        let mut note = Vec::with_capacity(n_windows * (WINDOW_FRAMES - 2 * trim) * PITCHES);
        let mut onset = Vec::with_capacity(note.capacity());
        let mut window = vec![0.0f32; WINDOW];
        for w in 0..n_windows {
            let start = w * hop;
            let end = (start + WINDOW).min(padded.len());
            window[..end - start].copy_from_slice(&padded[start..end]);
            window[end - start..].fill(0.0);
            let (n, o) = self.run_window(&window)?;
            note.extend_from_slice(&n[trim * PITCHES..(WINDOW_FRAMES - trim) * PITCHES]);
            onset.extend_from_slice(&o[trim * PITCHES..(WINDOW_FRAMES - trim) * PITCHES]);
            if !progress((w + 1) as f32 / n_windows as f32) {
                return Ok(None);
            }
        }
        let frames = (audio.len() / FFT_HOP).min(note.len() / PITCHES);
        note.truncate(frames * PITCHES);
        onset.truncate(frames * PITCHES);
        Ok(Some(Posteriors { frames, note, onset }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_time_matches_basic_pitch_constants() {
        assert_eq!(frame_time(0), 0.0);
        // window 1 starts at frame 172: 172 * 256 / 22050 = 1.99692 s minus the 10.3 ms drift
        let t = frame_time(172);
        assert!((t - (172.0 * 256.0 / 22050.0 - 0.010_30)).abs() < 1e-4, "{t}");
    }

    #[test]
    fn model_loads_and_silence_is_quiet() {
        let m = Model::load().expect("load");
        let p = m.posteriors(&vec![0.0; 22050 * 3], |_| true).unwrap().unwrap();
        assert_eq!(p.frames, 22050 * 3 / 256);
        let mx = p.onset.iter().cloned().fold(0.0, f32::max);
        assert!(mx < 0.3, "onset max {mx}");
    }
}
