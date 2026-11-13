//! Guitar decoder on Basic Pitch posteriors: strings, frets, re-plucks. A 1:1 port of
//! `guitar-hear/gh.py` (`decode`, DEFAULT parameters tuned on GuitarSet comp players 00-03).
//!
//! Six strings each sound one note at a time; a pitch is only possible where tuning + capo +
//! fret 0..=max_fret can produce it; the hand stays in a position, so of all string/fret
//! readings the cheapest path is taken; re-plucks come from local peaks of the onset head.

use crate::model::{frame_time, Posteriors, MIDI_OFFSET};
use crate::Note;

/// Open strings, low E first: E2 A2 D3 G3 B3 E4.
pub const STANDARD_TUNING: [u8; 6] = [40, 45, 50, 55, 59, 64];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuitarParams {
    pub on_thr: f32,
    pub min_gap: usize,
    /// An onset that is an overtone of a stronger lower one is dropped when `lower * harm >= v`.
    pub harm: f64,
    pub chord_win: usize,
    pub note_thr: f32,
    pub need_note: f32,
    pub capo: u8,
    pub max_fret: u8,
    pub tuning: [u8; 6],
}

impl Default for GuitarParams {
    fn default() -> Self {
        Self {
            on_thr: 0.5,
            min_gap: 4,
            harm: 0.8,
            chord_win: 3,
            note_thr: 0.3,
            need_note: 0.35,
            capo: 0,
            max_fret: 19,
            tuning: STANDARD_TUNING,
        }
    }
}

/// Local maxima of `x` at or above `thr`, at least `min_gap` frames apart (stronger wins).
fn peaks(x: &[f32], thr: f32, min_gap: usize) -> Vec<usize> {
    let mut cand: Vec<usize> =
        (1..x.len().saturating_sub(1)).filter(|&i| x[i] >= thr && x[i] >= x[i - 1] && x[i] > x[i + 1]).collect();
    cand.sort_by(|&a, &b| x[b].total_cmp(&x[a])); // stable: ties keep time order, like Python's sorted()
    let mut keep: Vec<usize> = Vec::new();
    for i in cand {
        if keep.iter().all(|&j| i.abs_diff(j) >= min_gap) {
            keep.push(i);
        }
    }
    keep.sort_unstable();
    keep
}

/// Onset events (frame, midi, strength) from the onset head, harmonics thinned.
fn candidates(post: &Posteriors, p: &GuitarParams) -> Vec<(usize, u8, f64)> {
    let mut ev: Vec<(usize, u8, f64)> = Vec::new();
    let mut col: Vec<f32> = Vec::with_capacity(post.frames);
    for m in 40usize..=88 {
        let k = m - MIDI_OFFSET;
        col.clear();
        col.extend((0..post.frames).map(|f| post.onset_at(f, k)));
        for f in peaks(&col, p.on_thr, p.min_gap) {
            // a real attack is followed by the note itself; a bleed/onset blip is not
            let end = (f + 4).min(post.frames);
            let mean = (f..end).map(|t| post.note_at(t, k)).sum::<f32>() / (end - f) as f32;
            if (mean as f64) < p.need_note as f64 {
                continue;
            }
            ev.push((f, m as u8, post.onset_at(f, k) as f64));
        }
    }
    ev.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.total_cmp(&b.2)));
    if p.harm > 0.0 {
        let all = ev.clone();
        ev.retain(|&(f, m, v)| {
            let lo = all.partition_point(|e| e.0 + 2 < f);
            let lower = all[lo..]
                .iter()
                .take_while(|e| e.0 <= f + 2)
                .filter(|e| matches!(m as i32 - e.1 as i32, 12 | 19 | 24 | 28))
                .map(|e| e.2)
                .fold(None, |acc: Option<f64>, x| Some(acc.map_or(x, |a| a.max(x))));
            !matches!(lower, Some(l) if l * p.harm >= v)
        });
    }
    ev
}

/// (string index 0 = low E, fret) pairs that can sound pitch `m`.
fn positions(m: u8, tuning: &[u8; 6], capo: u8, max_fret: u8) -> Vec<(usize, i32)> {
    tuning
        .iter()
        .enumerate()
        .filter_map(|(s, &open)| {
            let fret = m as i32 - (open as i32 + capo as i32);
            (0..=max_fret as i32).contains(&fret).then_some((s, fret))
        })
        .collect()
}

/// Strings for notes struck together: one string each, cost = distance of the frets from the hand
/// position, open strings free, a small cost for stealing a still-ringing string, 6.0 for dropping
/// a detected note. Enumerates in the same order as Python's `itertools.product` and keeps the
/// first minimum, so ties resolve identically.
fn assign_strings(
    cluster: &[(usize, u8, f64)],
    ringing: &[Option<usize>; 6],
    hand: f64,
    p: &GuitarParams,
) -> Vec<Option<(usize, i32)>> {
    let opts: Vec<Vec<(usize, i32)>> = cluster.iter().map(|&(_, m, _)| positions(m, &p.tuning, p.capo, p.max_fret)).collect();
    struct Search<'a> {
        opts: &'a [Vec<(usize, i32)>],
        ringing: &'a [Option<usize>; 6],
        hand: f64,
        best: Option<Vec<Option<(usize, i32)>>>,
        best_c: f64,
        cur: Vec<Option<(usize, i32)>>,
    }
    impl Search<'_> {
        fn go(&mut self, i: usize, used: u8, cost: f64) {
            if i == self.opts.len() {
                let frets: Vec<i32> = self.cur.iter().flatten().map(|c| c.1).filter(|&f| f > 0).collect();
                let mut cost = cost;
                if let (Some(&hi), Some(&lo)) = (frets.iter().max(), frets.iter().min()) {
                    cost += 0.6 * (hi - lo - 3).max(0) as f64; // a hand spans ~4 frets
                }
                if cost < self.best_c {
                    self.best_c = cost;
                    self.best = Some(self.cur.clone());
                }
                return;
            }
            let n_opts = self.opts[i].len();
            for oi in 0..=n_opts {
                if oi == n_opts {
                    self.cur.push(None);
                    self.go(i + 1, used, cost + 6.0); // dropping a detected note is expensive
                    self.cur.pop();
                    continue;
                }
                let (s, fret) = self.opts[i][oi];
                if used & (1 << s) != 0 {
                    continue;
                }
                let mut c = cost + if fret == 0 { 0.0 } else { 0.5 + 0.35 * (fret as f64 - self.hand).abs() };
                if self.ringing[s].is_some() {
                    c += 0.3;
                }
                self.cur.push(Some((s, fret)));
                self.go(i + 1, used | (1 << s), c);
                self.cur.pop();
            }
        }
    }
    let mut s = Search { opts: &opts, ringing, hand, best: None, best_c: 1e9, cur: Vec::new() };
    s.go(0, 0, 0.0);
    s.best.unwrap_or_else(|| vec![None; cluster.len()])
}

pub fn decode_guitar(post: &Posteriors, p: &GuitarParams) -> Vec<Note> {
    let ev = candidates(post, p);
    let t_len = post.frames;
    let mut out: Vec<Note> = Vec::new();
    let mut ringing: [Option<usize>; 6] = [None; 6];
    let mut hand = 1.0f64;
    let mut i = 0;
    while i < ev.len() {
        let mut j = i;
        while j + 1 < ev.len() && ev[j + 1].0 - ev[i].0 <= p.chord_win {
            j += 1;
        }
        let mut cluster: Vec<(usize, u8, f64)> = ev[i..=j].to_vec();
        if cluster.len() > 6 {
            // a guitar has six strings: keep the strongest
            cluster.sort_by(|a, b| b.2.total_cmp(&a.2));
            cluster.truncate(6);
        }
        let combo = assign_strings(&cluster, &ringing, hand, p);
        for (&(f, m, v), c) in cluster.iter().zip(combo) {
            let Some((s, fret)) = c else { continue };
            if let Some(prev) = ringing[s] {
                // one note per string: the old one stops here
                out[prev].end = out[prev].end.min(frame_time(f));
            }
            // the note lasts while the note head stays up (or until the string is re-used)
            let k = m as usize - MIDI_OFFSET;
            let mut e = f + 1;
            while e < t_len && post.note_at(e, k) as f64 > p.note_thr as f64 {
                e += 1;
            }
            out.push(Note {
                start: frame_time(f),
                end: frame_time(e.max(f + 3)),
                midi: m,
                vel: v as f32,
                velocity: (30.0 + 90.0 * v.min(1.0)) as u8,
                string: Some(6 - s as u8),
                fret: Some(fret as u8),
            });
            ringing[s] = Some(out.len() - 1);
            if fret > 0 {
                hand = 0.7 * hand + 0.3 * fret as f64;
            }
        }
        i = j + 1;
    }
    out
}
