//! "Any" decoder: Basic Pitch's default note creation (`basic_pitch/note_creation.py`,
//! `output_to_notes_polyphonic`) ported line by line: inferred onsets, backwards walk over
//! onset peaks, then the melodia pass over what is left. No pitch bends.

use crate::model::{frame_time, Posteriors, MIDI_OFFSET, PITCHES};
use crate::Note;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnyParams {
    /// Minimum onset activation (`--onset-threshold`).
    pub onset_thr: f32,
    /// Minimum frame activation for a note to stay on (`--frame-threshold`).
    pub frame_thr: f32,
    /// Shortest note, frames (`--minimum-note-length 127.7` ms = 11 frames).
    pub min_note_len: usize,
    pub infer_onsets: bool,
    pub melodia: bool,
    /// Frames below the threshold tolerated inside a note.
    pub energy_tol: usize,
}

impl Default for AnyParams {
    fn default() -> Self {
        Self { onset_thr: 0.5, frame_thr: 0.3, min_note_len: 11, infer_onsets: true, melodia: true, energy_tol: 11 }
    }
}

/// Onsets plus onsets inferred from big rises of the frame activation (`get_infered_onsets`,
/// n_diff = 2). Float64, as numpy does after concatenating with float64 zeros.
fn inferred_onsets(onsets: &[f32], frames: &[f32], t: usize) -> Vec<f64> {
    let mut fd = vec![0.0f64; t * PITCHES];
    for ti in 0..t {
        for p in 0..PITCHES {
            let cur = frames[ti * PITCHES + p] as f64;
            let d1 = cur - if ti >= 1 { frames[(ti - 1) * PITCHES + p] as f64 } else { 0.0 };
            let d2 = cur - if ti >= 2 { frames[(ti - 2) * PITCHES + p] as f64 } else { 0.0 };
            let d = d1.min(d2);
            fd[ti * PITCHES + p] = if d < 0.0 || ti < 2 { 0.0 } else { d };
        }
    }
    let max_on = onsets.iter().cloned().fold(f32::MIN, f32::max) as f64;
    let max_fd = fd.iter().cloned().fold(0.0f64, f64::max);
    onsets
        .iter()
        .zip(&fd)
        .map(|(&o, &d)| {
            let scaled = if max_fd > 0.0 { max_on * d / max_fd } else { 0.0 };
            (o as f64).max(scaled)
        })
        .collect()
}

/// Notes as (start frame, end frame, midi, amplitude).
pub fn notes_in_frames(post: &Posteriors, p: &AnyParams) -> Vec<(usize, usize, u8, f32)> {
    let n = post.frames;
    if n < 3 {
        return Vec::new();
    }
    let frames = &post.note;
    let onsets: Vec<f64> =
        if p.infer_onsets { inferred_onsets(&post.onset, frames, n) } else { post.onset.iter().map(|&v| v as f64).collect() };

    // onset peaks (argrelmax along time, strict) at or above the threshold, latest first
    let mut peaks: Vec<(usize, usize)> = Vec::new();
    for t in 1..n - 1 {
        for f in 0..PITCHES {
            let v = onsets[t * PITCHES + f];
            if v > onsets[(t - 1) * PITCHES + f] && v > onsets[(t + 1) * PITCHES + f] && v >= p.onset_thr as f64 {
                peaks.push((t, f));
            }
        }
    }
    peaks.reverse();

    let mut rem: Vec<f32> = frames.clone();
    let idx = |t: usize, f: usize| t * PITCHES + f;
    let zero_around = |rem: &mut Vec<f32>, t: usize, f: usize| {
        rem[idx(t, f)] = 0.0;
        if f < PITCHES - 1 {
            rem[idx(t, f + 1)] = 0.0;
        }
        if f > 0 {
            rem[idx(t, f - 1)] = 0.0;
        }
    };
    let mean = |a: usize, b: usize, f: usize| -> f32 { (a..b).map(|t| frames[idx(t, f)]).sum::<f32>() / (b - a) as f32 };

    let mut out = Vec::new();
    for (start, f) in peaks {
        if start >= n - 1 {
            continue;
        }
        let (mut i, mut k) = (start + 1, 0usize);
        while i < n - 1 && k < p.energy_tol {
            if rem[idx(i, f)] < p.frame_thr {
                k += 1;
            } else {
                k = 0;
            }
            i += 1;
        }
        i -= k;
        if i - start <= p.min_note_len {
            continue;
        }
        for t in start..i {
            zero_around(&mut rem, t, f);
        }
        out.push((start, i, (f + MIDI_OFFSET) as u8, mean(start, i, f)));
    }

    if p.melodia {
        // Python takes argmax of the whole matrix each round; the order is "strongest cell first,
        // lowest flat index on ties", and cells only ever drop to 0, so a sorted list is the same.
        let mut cand: Vec<(f32, usize)> =
            rem.iter().enumerate().filter(|(_, &v)| v > p.frame_thr).map(|(i, &v)| (v, i)).collect();
        cand.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then(a.1.cmp(&b.1)));
        for (_, flat) in cand {
            if rem[flat] <= p.frame_thr {
                continue;
            }
            let (i_mid, f) = (flat / PITCHES, flat % PITCHES);
            rem[flat] = 0.0;

            let (mut i, mut k) = (i_mid + 1, 0usize);
            while i < n - 1 && k < p.energy_tol {
                if rem[idx(i, f)] < p.frame_thr {
                    k += 1;
                } else {
                    k = 0;
                }
                zero_around(&mut rem, i, f);
                i += 1;
            }
            let i_end = (i - 1) as i64 - k as i64;

            let (mut i, mut k) = (i_mid as i64 - 1, 0usize);
            while i > 0 && k < p.energy_tol {
                if rem[idx(i as usize, f)] < p.frame_thr {
                    k += 1;
                } else {
                    k = 0;
                }
                zero_around(&mut rem, i as usize, f);
                i -= 1;
            }
            let i_start = i + 1 + k as i64;

            if i_end - i_start <= p.min_note_len as i64 || i_start < 0 {
                continue;
            }
            let (a, b) = (i_start as usize, i_end as usize);
            out.push((a, b, (f + MIDI_OFFSET) as u8, mean(a, b, f)));
        }
    }
    out
}

/// Polyphonic notes for any pitched instrument, sorted by start time.
pub fn decode_any(post: &Posteriors, p: &AnyParams) -> Vec<Note> {
    let mut notes: Vec<Note> = notes_in_frames(post, p)
        .into_iter()
        .map(|(a, b, midi, amp)| Note {
            start: frame_time(a),
            end: frame_time(b),
            midi,
            vel: amp,
            velocity: ((127.0 * amp).round() as i32).clamp(1, 127) as u8,
            string: None,
            fret: None,
        })
        .collect();
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.midi.cmp(&b.midi)));
    notes
}
