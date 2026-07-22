//! Phrase engine (v2.3) — the executive layer for vocal *dialogue* on one
//! beat. Two modes share this machinery:
//!
//! - **EDL** (`[[phrase]]`) — an explicit cut list: each phrase is a source
//!   slice placed at an exact grid beat, optionally pitch-shifted, looped and
//!   delay-thrown. Overlaps are allowed (they sum).
//! - **pingpong** (`[pingpong]`) — auto-segment two vocals into phrases and
//!   alternate them A/B/A/B on the grid, with seeded random stutter-loops and
//!   chipmunk / slowed pitch answers.
//!
//! Shared primitives: [`pitch_resample`] (chipmunk-style resample pitch) and
//! [`segment_phrases`] (onset-gap phrase boundaries — a breath > `gap_sec`).

use crate::config::{PhraseConfig, PingpongConfig};
use crate::duck::db_to_lin;
use superduper_synth_core::dsp_blocks::Biquad;

/// A rendered phrase ready to place on the timeline.
pub struct PlacedPhrase {
    pub offset: usize,
    pub gain: f32,
    pub l: Vec<f32>,
    pub r: Vec<f32>,
}

/// A named, decoded vocal source the phrase engine cuts from.
#[derive(Clone)]
pub struct SourceAudio {
    pub name: String,
    pub l: Vec<f32>,
    pub r: Vec<f32>,
}

impl SourceAudio {
    fn frames(&self) -> usize {
        self.l.len().min(self.r.len())
    }
    /// Cut `[start_sec, end_sec)` to a fresh stereo pair (clamped to the file).
    fn cut(&self, start_sec: f64, end_sec: f64, sr: u32) -> (Vec<f32>, Vec<f32>) {
        let n = self.frames();
        let s0 = ((start_sec.max(0.0) * sr as f64).round() as usize).min(n);
        let s1 = ((end_sec.max(0.0) * sr as f64).round() as usize).clamp(s0, n);
        (self.l[s0..s1].to_vec(), self.r[s0..s1].to_vec())
    }
}

fn find<'a>(sources: &'a [SourceAudio], name: &str) -> Option<&'a SourceAudio> {
    sources.iter().find(|s| s.name == name)
}

/// Beats → sample offset on the grid (rounds to the nearest sample).
fn beat_to_samples(beat: f64, bpm: f64, sr: u32) -> usize {
    (beat.max(0.0) * (60.0 / bpm) * sr as f64).round() as usize
}

/// Repeat a phrase back-to-back `times` times (EDL `loops` = a bar-length
/// stutter / echo of the whole slice). Repeat k plays at `k * step_db` so a
/// looped hook can escalate toward its exit (0 = flat).
fn repeat(l: &[f32], r: &[f32], times: u32, step_db: f64) -> (Vec<f32>, Vec<f32>) {
    let times = times.max(1) as usize;
    if times == 1 {
        return (l.to_vec(), r.to_vec());
    }
    let mut ol = Vec::with_capacity(l.len() * times);
    let mut or = Vec::with_capacity(r.len() * times);
    for k in 0..times {
        let g = db_to_lin((k as f64 * step_db) as f32);
        ol.extend(l.iter().map(|v| v * g));
        or.extend(r.iter().map(|v| v * g));
    }
    (ol, or)
}

/// Stutter-fill: grab the first `grab` samples and repeat them until `total`
/// samples are produced — the machine-gun vocal stutter in the pingpong.
fn stutter_fill(l: &[f32], r: &[f32], grab: usize, total: usize) -> (Vec<f32>, Vec<f32>) {
    let n = l.len().min(r.len());
    let grab = grab.clamp(1, n.max(1)).min(n);
    if grab == 0 || total == 0 {
        return (l.to_vec(), r.to_vec());
    }
    let mut ol = Vec::with_capacity(total);
    let mut or = Vec::with_capacity(total);
    while ol.len() < total {
        let take = (total - ol.len()).min(grab);
        ol.extend_from_slice(&l[..take]);
        or.extend_from_slice(&r[..take]);
    }
    (ol, or)
}

/// Throw the phrase tail into a dotted-quarter feedback echo (extends the
/// buffer). Additive — the dry phrase is untouched. Same dub tail as the
/// per-track `delay_throw`, but scoped to one phrase (no envelope detection).
fn apply_phrase_throw(l: &mut Vec<f32>, r: &mut Vec<f32>, sr: u32, bpm: f64, feedback: f32) {
    let n = l.len().min(r.len());
    let beat = (60.0 / bpm) * sr as f64;
    let delay = (1.5 * beat).round() as usize; // dotted quarter
    let throw = ((sr as f64) * 0.32).round() as usize; // the last "word"
    let fb = feedback.clamp(0.0, 0.85);
    if n < throw || delay == 0 || throw == 0 {
        return;
    }
    let seed_l: Vec<f32> = l[n - throw..n].to_vec();
    let seed_r: Vec<f32> = r[n - throw..n].to_vec();
    // Room for the decaying echoes.
    let tail = delay * 6 + throw;
    l.resize(n + tail, 0.0);
    r.resize(n + tail, 0.0);
    let mut k = 1usize;
    loop {
        let g = fb.powi(k as i32);
        if g < 0.06 {
            break;
        }
        let base = n + (k - 1) * delay;
        if base >= l.len() {
            break;
        }
        for j in 0..throw {
            let d = base + j;
            if d < l.len() {
                l[d] += seed_l[j] * g;
                r[d] += seed_r[j] * g;
            }
        }
        k += 1;
    }
}

/// Build placed phrases from an explicit EDL list (`[[phrase]]`). Each phrase
/// is cut, optionally pitched (resample), looped, and delay-thrown, then placed
/// at its grid beat. An empty/zero-length cut is skipped. Overlaps are the
/// caller's concern (they sum in the vocal bus).
pub fn build_edl(
    phrases: &[PhraseConfig],
    sources: &[SourceAudio],
    bpm: f64,
    sr: u32,
) -> Result<Vec<PlacedPhrase>, String> {
    let mut out = Vec::with_capacity(phrases.len());
    for ph in phrases {
        let src = find(sources, &ph.track)
            .ok_or_else(|| format!("[[phrase]] references unknown source '{}'", ph.track))?;
        let (mut cl, mut cr) = src.cut(ph.start_sec, ph.end_sec, sr);
        if cl.is_empty() {
            continue;
        }
        if ph.pitch_semitones.abs() > 1e-6 {
            let (pl, pr) = pitch_resample(&cl, &cr, ph.pitch_semitones);
            cl = pl;
            cr = pr;
        }
        if ph.loops > 1 {
            let (rl, rr) = repeat(&cl, &cr, ph.loops, ph.loop_gain_step_db);
            cl = rl;
            cr = rr;
        }
        if ph.throw {
            apply_phrase_throw(&mut cl, &mut cr, sr, bpm, 0.5);
        }
        out.push(PlacedPhrase {
            offset: beat_to_samples(ph.at_beat, bpm, sr),
            gain: db_to_lin(ph.gain_db as f32),
            l: cl,
            r: cr,
        });
    }
    Ok(out)
}

/// Auto vocal ping-pong (`[pingpong]`). Segment both vocals into onset-gap
/// phrases and alternate them A/B/A/B on the grid at `phrase_beats` spacing.
/// A seeded PRNG decides per phrase whether to stutter-loop (`loop_prob`),
/// pitch it chipmunk-up / slowed-down (`pitch_prob`), and occasionally overlap
/// it back onto its neighbour. Deterministic per `seed`.
pub fn build_pingpong(
    cfg: &PingpongConfig,
    sources: &[SourceAudio],
    bpm: f64,
    sr: u32,
) -> Result<Vec<PlacedPhrase>, String> {
    if cfg.vocals.len() < 2 {
        return Err("[pingpong] needs exactly two vocal source names".into());
    }
    let a = find(sources, &cfg.vocals[0])
        .ok_or_else(|| format!("[pingpong] unknown vocal '{}'", cfg.vocals[0]))?;
    let b = find(sources, &cfg.vocals[1])
        .ok_or_else(|| format!("[pingpong] unknown vocal '{}'", cfg.vocals[1]))?;

    let spb = 60.0 / bpm;
    let step = cfg.phrase_beats.max(0.25);
    let step_samples = (step * spb * sr as f64).round().max(1.0) as usize;
    let grab_samples = ((step * 0.5) * spb * sr as f64).round().max(1.0) as usize;
    // A mandatory breath at the end of every slot: without it back-to-back
    // rap hand-overs read as one continuous mush ("каша") even with zero
    // literal overlap — the ear needs ~150 ms to register the voice change.
    let gap_samples = ((cfg.gap_ms.max(0.0) / 1000.0) * sr as f64).round() as usize;
    let chunk_max = step_samples
        .saturating_sub(gap_samples)
        .max((sr as usize / 10).min(step_samples));

    // Segment each vocal at breath gaps, then split long passages into
    // ≤ (slot − gap) chunks. Rap has few 0.35 s gaps, so onset-gap segmentation
    // alone yields 15-30 s blobs — placed one slot apart they pile up and play
    // *simultaneously* (the "они вместе играют" bug). Chunking makes every
    // phrase fit its slot (breath included) so the two voices trade bar-by-bar.
    let pa = chunk_phrases(a, &segment_phrases(&a.l, &a.r, sr, 0.20, 0.30), chunk_max, sr);
    let pb = chunk_phrases(b, &segment_phrases(&b.l, &b.r, sr, 0.20, 0.30), chunk_max, sr);
    if pa.is_empty() && pb.is_empty() {
        return Ok(Vec::new());
    }

    // Decorrelate the seed so seed 0 isn't a degenerate PRNG state.
    let mut rng = Rng::new(cfg.seed ^ 0x9E37_79B9_7F4A_7C15);
    let gain = db_to_lin(cfg.gain_db as f32);

    let dbg = std::env::var("SDSP_DEBUG_PINGPONG").is_ok();
    let mut out = Vec::new();
    let (mut ia, mut ib) = (0usize, 0usize);
    let mut beat = cfg.start_beat;
    let mut turn_a = true;

    while ia < pa.len() || ib < pb.len() {
        // The beat outlives the dialogue, never the other way round.
        if let Some(end) = cfg.end_beat {
            if beat + step > end {
                break;
            }
        }
        // Take from the vocal whose turn it is; fall back to the other when the
        // wanted one is exhausted (guarantees one index advances per iteration).
        let use_a = if turn_a { ia < pa.len() } else { ib >= pb.len() };
        let (src, phr, idx, name) = if use_a {
            (a, &pa, &mut ia, &cfg.vocals[0])
        } else {
            (b, &pb, &mut ib, &cfg.vocals[1])
        };
        let (s0, s1) = phr[*idx];
        *idx += 1;

        let mut cl = src.l[s0..s1].to_vec();
        let mut cr = src.r[s0..s1].to_vec();
        let mut pitched = 0.0f64;

        // Chipmunk-up or slowed-down answer.
        if rng.chance(cfg.pitch_prob) {
            let semis = if rng.chance(0.5) {
                5.0 + rng.unit() * 2.0 // +5..+7 chipmunk
            } else {
                -(3.0 + rng.unit() * 2.0) // −3..−5 slowed
            };
            let (pl, pr) = pitch_resample(&cl, &cr, semis);
            cl = pl;
            cr = pr;
            pitched = semis;
        }

        // Machine-gun stutter over the slot (minus the breath gap).
        if rng.chance(cfg.loop_prob) {
            let (sl, srr) = stutter_fill(&cl, &cr, grab_samples, chunk_max);
            cl = sl;
            cr = srr;
        }

        // Advance the timeline PAST this phrase, quantised up to a whole slot,
        // so the next (other-voice) phrase can never overlap this one. A pitch-
        // slowed phrase that grew past one slot simply takes two. Measured
        // BEFORE the optional throw — the echo tail is allowed to ring softly
        // under the next voice, it must not push the grid.
        let dur_beats = cl.len() as f64 / (spb * sr as f64);
        let slots = (dur_beats / step).ceil().max(1.0);

        // Soft dub throw (off by default — the tail blurs dense rap).
        if rng.chance(cfg.throw_prob) {
            apply_phrase_throw(&mut cl, &mut cr, sr, bpm, 0.35);
        }

        // Perceptual voice split: A slightly left + darker, B slightly right
        // + brighter, so two similar rap timbres stay trackable.
        let pan = cfg.voice_pan.clamp(0.0, 1.0) as f32;
        if pan > 0.0 {
            let (gl, gr) = if use_a { (1.0, 1.0 - pan) } else { (1.0 - pan, 1.0) };
            for v in cl.iter_mut() {
                *v *= gl;
            }
            for v in cr.iter_mut() {
                *v *= gr;
            }
        }
        let tilt = if use_a { -cfg.voice_tilt_db } else { cfg.voice_tilt_db };
        if tilt.abs() > 1e-6 {
            let mut fl = Biquad::default();
            let mut fr = Biquad::default();
            fl.set_high_shelf(sr as f32, 5000.0, 1.0, tilt as f32);
            fr.set_high_shelf(sr as f32, 5000.0, 1.0, tilt as f32);
            for v in cl.iter_mut() {
                *v = fl.process(*v);
            }
            for v in cr.iter_mut() {
                *v = fr.process(*v);
            }
        }

        let off = beat_to_samples(beat, bpm, sr);
        if dbg {
            eprintln!(
                "[pp] {name:>8} beat {beat:6.2}  at {:7.3}s  dur {:6.3}s  pitch {pitched:+.1}  src [{:.3}..{:.3}]s",
                off as f64 / sr as f64,
                dur_beats * spb,
                s0 as f64 / sr as f64,
                s1 as f64 / sr as f64,
            );
        }

        out.push(PlacedPhrase {
            offset: off,
            gain,
            l: cl,
            r: cr,
        });

        beat += slots * step;
        turn_a = !turn_a;
    }
    Ok(out)
}

/// Split each breath-gap phrase into consecutive chunks of at most `max_len`
/// samples and drop chunks that are near-silent or shorter than ~0.12 s
/// (don't trade a breath). This turns a continuous 30 s rap passage into
/// one-slot phrases so the ping-pong actually alternates instead of dumping
/// the whole verse.
///
/// Where to cut matters as much as how long: a hard cut at `max_len` lands
/// mid-word ~2/3 of the time on rap ("рваный шум"). So each cut first looks
/// for a natural energy dip (an inter-word gap too short for
/// `segment_phrases`) inside `[max_len/2, max_len]` and cuts there; the hard
/// ceiling is only the fallback for genuinely continuous audio.
fn chunk_phrases(
    src: &SourceAudio,
    phrases: &[(usize, usize)],
    max_len: usize,
    sr: u32,
) -> Vec<(usize, usize)> {
    let max_len = max_len.max(1);
    let min_len = ((sr as f64) * 0.12) as usize;
    let floor = 0.004f32; // ≈ −48 dBFS
    let mut out = Vec::new();
    for &(s0, s1) in phrases {
        let mut a = s0;
        while a < s1 {
            let hard = (a + max_len).min(s1);
            // Only search for a dip when we're actually splitting mid-phrase;
            // the final remainder keeps its natural (segmented) end.
            let b = if hard < s1 { find_dip_cut(src, a, max_len, sr).unwrap_or(hard) } else { hard };
            let n = b - a;
            if n >= min_len {
                let mut sq = 0.0f64;
                for i in a..b {
                    let v = 0.5 * (src.l[i] + src.r[i]);
                    sq += (v as f64) * (v as f64);
                }
                if (sq / n as f64).sqrt() as f32 >= floor {
                    out.push((a, b));
                }
            }
            a = b;
        }
    }
    out
}

/// Find a natural cut point inside `[a + max_len/2, a + max_len)`: the centre
/// of the quietest ~30 ms RMS window, accepted only when it dips ≥ 6 dB below
/// the search range's median level (a real inter-word gap, not just a softer
/// vowel). Returns `None` for continuous material — the caller hard-cuts.
fn find_dip_cut(src: &SourceAudio, a: usize, max_len: usize, sr: u32) -> Option<usize> {
    let lo = a + max_len / 2;
    let hi = (a + max_len).min(src.frames());
    let hop = (sr as usize / 200).max(1); // 5 ms scan
    let win = (sr as usize * 3 / 100).max(1); // 30 ms RMS window
    if hi <= lo + win {
        return None;
    }
    let mut env: Vec<(f32, usize)> = Vec::with_capacity((hi - lo) / hop + 1);
    let mut p = lo;
    while p + win <= hi {
        let mut sq = 0.0f64;
        for i in p..p + win {
            let v = 0.5 * (src.l[i] + src.r[i]);
            sq += (v as f64) * (v as f64);
        }
        env.push(((sq / win as f64).sqrt() as f32, p));
        p += hop;
    }
    if env.len() < 4 {
        return None;
    }
    let mut vals: Vec<f32> = env.iter().map(|e| e.0).collect();
    vals.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let median = vals[vals.len() / 2];
    let &(min_rms, min_pos) =
        env.iter().min_by(|x, y| x.0.partial_cmp(&y.0).unwrap()).unwrap();
    (min_rms <= median * 0.5).then_some(min_pos + win / 2)
}

/// Resample-based pitch shift (changes pitch *and* duration — the chipmunk
/// effect Fred-again scatters use). `+semitones` → higher + shorter.
pub fn pitch_resample(l: &[f32], r: &[f32], semitones: f64) -> (Vec<f32>, Vec<f32>) {
    let n = l.len().min(r.len());
    if semitones.abs() < 1e-6 || n == 0 {
        return (l[..n].to_vec(), r[..n].to_vec());
    }
    let ratio = 2f64.powf(semitones / 12.0); // read step: >1 = up + shorter
    let out_len = ((n as f64) / ratio).floor() as usize;
    let mut ol = Vec::with_capacity(out_len);
    let mut or = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * ratio;
        let j = pos.floor() as usize;
        let f = (pos - j as f64) as f32;
        let (l0, l1) = (l[j], *l.get(j + 1).unwrap_or(&l[j]));
        let (r0, r1) = (r[j], *r.get(j + 1).unwrap_or(&r[j]));
        ol.push(l0 * (1.0 - f) + l1 * f);
        or.push(r0 * (1.0 - f) + r1 * f);
    }
    (ol, or)
}

/// Onset-gap phrase segmentation. Returns `[start, end)` sample ranges of the
/// loud regions (words / lines) separated by silences longer than `gap_sec`.
/// Phrases shorter than `min_phrase_sec` are dropped.
pub fn segment_phrases(
    l: &[f32],
    r: &[f32],
    sr: u32,
    gap_sec: f64,
    min_phrase_sec: f64,
) -> Vec<(usize, usize)> {
    let n = l.len().min(r.len());
    let hop = (sr as usize / 100).max(1); // 100 Hz envelope
    let frames = n / hop;
    if frames < 4 {
        return Vec::new();
    }
    let mut env = vec![0.0f32; frames];
    let mut peak = 1e-9f32;
    for (f, e) in env.iter_mut().enumerate() {
        let base = f * hop;
        let mut sq = 0.0f32;
        for k in 0..hop {
            let v = 0.5 * (l[base + k] + r[base + k]);
            sq += v * v;
        }
        *e = (sq / hop as f32).sqrt();
        peak = peak.max(*e);
    }
    let thr = peak * 0.12;
    let gap_frames = ((gap_sec * 100.0) as usize).max(1);
    let min_frames = ((min_phrase_sec * 100.0) as usize).max(1);

    let mut phrases = Vec::new();
    let mut i = 0usize;
    while i < frames {
        if env[i] > thr {
            let start = i;
            // Extend until a gap of `gap_frames` sustained silence.
            let mut j = i;
            let mut silence = 0usize;
            while j < frames {
                if env[j] <= thr {
                    silence += 1;
                    if silence >= gap_frames {
                        break;
                    }
                } else {
                    silence = 0;
                }
                j += 1;
            }
            let end = (j - silence).max(start + 1);
            if end - start >= min_frames {
                phrases.push((start * hop, (end * hop).min(n)));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    phrases
}

/// A tiny deterministic PRNG (xorshift) so pingpong is reproducible per seed.
pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        (x >> 40) as f64 / (1u64 << 24) as f64
    }
    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SR: u32 = 44_100;

    #[test]
    fn pitch_up_shortens_and_raises() {
        // A 200 Hz tone pitched +12 st (octave) → ~half length, ~400 Hz.
        let n = SR as usize;
        let x: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * i as f32 / SR as f32).sin())
            .collect();
        let (o, _) = pitch_resample(&x, &x, 12.0);
        assert!(
            (o.len() as f64 - n as f64 / 2.0).abs() < n as f64 * 0.02,
            "octave up should ~halve length: {} vs {}",
            o.len(),
            n / 2
        );
        // Count zero crossings → frequency roughly doubled.
        let zc = |s: &[f32]| s.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        let f_in = zc(&x) as f64 / (n as f64 / SR as f64);
        let f_out = zc(&o) as f64 / (o.len() as f64 / SR as f64);
        assert!((f_out / f_in - 2.0).abs() < 0.1, "freq should double: {f_in}→{f_out}");
    }

    /// A source with `n_bursts` 0.4 s tone bursts, one per second (0.6 s gaps).
    fn burst_source(name: &str, freq: f32, n_bursts: usize) -> SourceAudio {
        let n = SR as usize * n_bursts;
        let mut x = vec![0.0f32; n];
        for p in 0..n_bursts {
            let start = p * SR as usize;
            for k in 0..(SR as usize * 4 / 10) {
                x[start + k] =
                    0.6 * (2.0 * std::f32::consts::PI * freq * k as f32 / SR as f32).sin();
            }
        }
        SourceAudio { name: name.to_string(), l: x.clone(), r: x }
    }

    #[test]
    fn edl_cuts_and_places_sample_accurate() {
        // Source: a lone impulse at 1.000 s (sample 44_100), 2 s long.
        let n = SR as usize * 2;
        let mut l = vec![0.0f32; n];
        l[SR as usize] = 1.0;
        let src = SourceAudio { name: "a".into(), l: l.clone(), r: l };

        // Cut [0.9, 1.1) s → the impulse sits at local 0.1 s = sample 4_410.
        // Place at beat 4 @ 120 BPM = 2.0 s = 88_200 samples.
        let phrases = vec![PhraseConfig {
            track: "a".into(),
            start_sec: 0.9,
            end_sec: 1.1,
            at_beat: 4.0,
            pitch_semitones: 0.0,
            loops: 1,
            loop_gain_step_db: 0.0,
            throw: false,
            gain_db: 0.0,
        }];
        let placed = build_edl(&phrases, std::slice::from_ref(&src), 120.0, SR).unwrap();
        assert_eq!(placed.len(), 1);
        let p = &placed[0];
        assert_eq!(p.offset, 88_200, "beat-4 offset @120 BPM");
        // Cut length = 0.2 s = 8_820 samples.
        assert_eq!(p.l.len(), 8_820, "cut length");
        // The impulse's local index inside the cut = 1.0 s − 0.9 s = 4_410.
        let peak = p
            .l
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().partial_cmp(&b.1.abs()).unwrap())
            .unwrap()
            .0;
        assert_eq!(peak, 4_410, "impulse inside the cut");
        // Absolute placement = 88_200 + 4_410 = 92_610.
    }

    #[test]
    fn edl_loops_repeat_the_slice() {
        let src = burst_source("a", 300.0, 1); // one 0.4 s burst at t=0
        let one = vec![PhraseConfig {
            track: "a".into(),
            start_sec: 0.0,
            end_sec: 0.5,
            at_beat: 0.0,
            pitch_semitones: 0.0,
            loops: 1,
            loop_gain_step_db: 0.0,
            throw: false,
            gain_db: 0.0,
        }];
        let three = vec![PhraseConfig { loops: 3, ..one[0].clone() }];
        let p1 = build_edl(&one, std::slice::from_ref(&src), 120.0, SR).unwrap();
        let p3 = build_edl(&three, std::slice::from_ref(&src), 120.0, SR).unwrap();
        assert_eq!(p3[0].l.len(), 3 * p1[0].l.len(), "loops=3 triples the length");
    }

    #[test]
    fn edl_loop_gain_step_escalates() {
        // loops=3 + loop_gain_step_db=2 → each repeat plays ~2 dB louder than
        // the previous, so a looped hook bridge builds instead of flat-lining.
        let src = burst_source("a", 300.0, 1);
        let phrases = vec![PhraseConfig {
            track: "a".into(),
            start_sec: 0.0,
            end_sec: 0.4,
            at_beat: 0.0,
            pitch_semitones: 0.0,
            loops: 3,
            loop_gain_step_db: 2.0,
            throw: false,
            gain_db: 0.0,
        }];
        let placed = build_edl(&phrases, std::slice::from_ref(&src), 120.0, SR).unwrap();
        let p = &placed[0];
        let unit = p.l.len() / 3;
        let rms = |x: &[f32]| {
            (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len() as f64).sqrt()
        };
        let (r1, r2, r3) =
            (rms(&p.l[..unit]), rms(&p.l[unit..2 * unit]), rms(&p.l[2 * unit..3 * unit]));
        let db = |a: f64, b: f64| 20.0 * (b / a).log10();
        assert!(
            (db(r1, r2) - 2.0).abs() < 0.3,
            "repeat 2 should be +2 dB over repeat 1, got {:+.2} dB",
            db(r1, r2)
        );
        assert!(
            (db(r2, r3) - 2.0).abs() < 0.3,
            "repeat 3 should be +2 dB over repeat 2, got {:+.2} dB",
            db(r2, r3)
        );
    }

    #[test]
    fn edl_unknown_source_errors() {
        let src = burst_source("a", 300.0, 1);
        let phrases = vec![PhraseConfig {
            track: "missing".into(),
            start_sec: 0.0,
            end_sec: 0.5,
            at_beat: 0.0,
            pitch_semitones: 0.0,
            loops: 1,
            loop_gain_step_db: 0.0,
            throw: false,
            gain_db: 0.0,
        }];
        assert!(build_edl(&phrases, &[src], 120.0, SR).is_err());
    }

    #[test]
    fn pingpong_is_reproducible_per_seed() {
        let a = burst_source("a", 300.0, 3);
        let b = burst_source("b", 500.0, 3);
        let cfg = PingpongConfig {
            vocals: vec!["a".into(), "b".into()],
            phrase_beats: 2.0,
            loop_prob: 0.5,
            pitch_prob: 0.5,
            seed: 42,
            start_beat: 0.0,
            end_beat: None,
            gain_db: 0.0,
            gap_ms: 150.0,
            voice_pan: 0.0,
            voice_tilt_db: 0.0,
            throw_prob: 0.0,
        };
        let sources = vec![a, b];
        let p1 = build_pingpong(&cfg, &sources, 120.0, SR).unwrap();
        let p2 = build_pingpong(&cfg, &sources, 120.0, SR).unwrap();
        assert!(p1.len() >= 4, "should place both vocals' phrases, got {}", p1.len());
        assert_eq!(p1.len(), p2.len(), "same seed → same count");
        for (x, y) in p1.iter().zip(&p2) {
            assert_eq!(x.offset, y.offset, "same seed → same offsets");
            assert_eq!(x.l.len(), y.l.len(), "same seed → same lengths");
            assert_eq!(x.l, y.l, "same seed → sample-identical");
        }
    }

    #[test]
    fn pingpong_alternates_and_advances_on_the_grid() {
        // No randomness (loop/pitch prob 0) → clean A/B alternation, one phrase
        // per `phrase_beats` slot. 2 beats @ 120 BPM = 1.0 s = 44_100 samples.
        let a = burst_source("a", 300.0, 2);
        let b = burst_source("b", 500.0, 2);
        let cfg = PingpongConfig {
            vocals: vec!["a".into(), "b".into()],
            phrase_beats: 2.0,
            loop_prob: 0.0,
            pitch_prob: 0.0,
            seed: 7,
            start_beat: 0.0,
            end_beat: None,
            gain_db: 0.0,
            gap_ms: 150.0,
            voice_pan: 0.0,
            voice_tilt_db: 0.0,
            throw_prob: 0.0,
        };
        let placed = build_pingpong(&cfg, &[a, b], 120.0, SR).unwrap();
        assert_eq!(placed.len(), 4, "2 phrases each → 4 slots");
        // First slot at the start; the last slot ≥ 2 full 44_100-sample steps
        // ahead (robust to the occasional ≤0.5-beat overlap-nudge).
        assert!(placed[0].offset <= 11_025, "first slot near the start");
        assert!(placed[3].offset >= 2 * 44_100, "slots advance on the grid");
    }

    #[test]
    fn segment_finds_phrases_between_gaps() {
        // Three 0.4 s tone bursts separated by 0.6 s silences.
        let mut x = vec![0.0f32; SR as usize * 3];
        for p in 0..3 {
            let start = p * SR as usize; // burst every 1 s
            for k in 0..(SR as usize * 4 / 10) {
                x[start + k] = 0.6 * (2.0 * std::f32::consts::PI * 300.0 * k as f32 / SR as f32).sin();
            }
        }
        let ph = segment_phrases(&x, &x, SR, 0.4, 0.1);
        assert_eq!(ph.len(), 3, "should find 3 phrases, got {}", ph.len());
        // Each phrase starts near a second boundary.
        for (i, &(s, _)) in ph.iter().enumerate() {
            let sec = s as f64 / SR as f64;
            assert!((sec - i as f64).abs() < 0.1, "phrase {i} at {sec}s");
        }
    }

    /// A solid, gap-free tone — the "continuous rap delivery" case that
    /// onset-gap segmentation collapses into one giant phrase.
    fn continuous_source(name: &str, freq: f32, secs: f64) -> SourceAudio {
        let n = (secs * SR as f64) as usize;
        let x: Vec<f32> = (0..n)
            .map(|i| 0.6 * (2.0 * std::f32::consts::PI * freq * i as f32 / SR as f32).sin())
            .collect();
        SourceAudio { name: name.to_string(), l: x.clone(), r: x }
    }

    #[test]
    fn pingpong_phrases_never_overlap() {
        // Two long CONTINUOUS vocals (no breath gaps) — the rap case that used
        // to segment into one ~20 s blob each, then pile up playing at the same
        // time ("они вместе играют"). Chunking + advance-by-actual-length must
        // yield strictly non-overlapping phrases.
        let a = continuous_source("a", 200.0, 18.0);
        let b = continuous_source("b", 800.0, 18.0);
        let cfg = PingpongConfig {
            vocals: vec!["a".into(), "b".into()],
            phrase_beats: 4.0,
            loop_prob: 0.0,
            pitch_prob: 0.0,
            seed: 3,
            start_beat: 0.0,
            end_beat: None,
            gain_db: 0.0,
            gap_ms: 150.0,
            voice_pan: 0.0,
            voice_tilt_db: 0.0,
            throw_prob: 0.0,
        };
        let placed = build_pingpong(&cfg, &[a, b], 120.0, SR).unwrap();
        assert!(
            placed.len() > 10,
            "long verses should chunk into many phrases, got {}",
            placed.len()
        );
        // Sort by start; no phrase may end after the next one starts (touch ok).
        let mut spans: Vec<(usize, usize)> =
            placed.iter().map(|p| (p.offset, p.offset + p.l.len())).collect();
        spans.sort_by_key(|s| s.0);
        let tol = SR as usize / 100; // 10 ms rounding slack
        for w in spans.windows(2) {
            assert!(
                w[0].1 <= w[1].0 + tol,
                "phrases overlap: {:?} ends after {:?} starts",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn pingpong_keeps_both_voices_to_the_end() {
        // Both voices must still be trading in the SECOND half of the timeline.
        // The old bug exhausted one source's 1-2 giant phrases early and left
        // the rest of the track to the other voice alone ("потом пропадает").
        let a = continuous_source("a", 150.0, 16.0); // low tone
        let b = continuous_source("b", 900.0, 16.0); // high tone
        let cfg = PingpongConfig {
            vocals: vec!["a".into(), "b".into()],
            phrase_beats: 4.0,
            loop_prob: 0.0,
            pitch_prob: 0.0,
            seed: 9,
            start_beat: 0.0,
            end_beat: None,
            gain_db: 0.0,
            gap_ms: 150.0,
            voice_pan: 0.0,
            voice_tilt_db: 0.0,
            throw_prob: 0.0,
        };
        let placed = build_pingpong(&cfg, &[a, b], 120.0, SR).unwrap();
        // Classify each phrase by its tone (rising zero-crossings ≈ frequency).
        let is_high = |p: &PlacedPhrase| {
            let zc = p.l.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
            (zc as f64 / (p.l.len() as f64 / SR as f64)) > 450.0
        };
        let half = placed.len() / 2;
        let (mut lo2, mut hi2) = (0usize, 0usize);
        for p in &placed[half..] {
            if is_high(p) {
                hi2 += 1;
            } else {
                lo2 += 1;
            }
        }
        assert!(
            lo2 > 0 && hi2 > 0,
            "both voices must persist into the 2nd half: low {lo2}, high {hi2}"
        );
    }

    #[test]
    fn chunk_cuts_at_energy_dip_not_at_ceiling() {
        // 4 s continuous tone with a 100 ms silence at 1.55 s — too short for
        // segment_phrases (gap 0.20 s) but a real inter-word breath. The
        // chunker must cut inside the dip, not at the hard ceiling.
        let n = 4 * SR as usize;
        let mut x: Vec<f32> = (0..n)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / SR as f32).sin())
            .collect();
        let dip0 = (1.55 * SR as f64) as usize;
        let dip1 = (1.65 * SR as f64) as usize;
        for v in &mut x[dip0..dip1] {
            *v = 0.0;
        }
        let src = SourceAudio { name: "a".into(), l: x.clone(), r: x };
        let max_len = (1.85 * SR as f64) as usize; // 2 s slot − 150 ms breath
        let chunks = chunk_phrases(&src, &[(0, n)], max_len, SR);
        assert!(chunks.len() >= 2, "expected multiple chunks, got {}", chunks.len());
        let end0 = chunks[0].1 as f64 / SR as f64;
        assert!(
            (1.53..=1.67).contains(&end0),
            "first cut should land in the dip, got {end0:.3}s (ceiling would be 1.850)"
        );
        // The continuous stretch after the dip has no gap → hard-cut fallback.
        let len1 = (chunks[1].1 - chunks[1].0) as f64 / SR as f64;
        assert!((len1 - 1.85).abs() < 0.02, "no dip → hard ceiling cut, got {len1:.3}s");
    }

    #[test]
    fn pingpong_leaves_breath_between_turns() {
        // Every voice hand-over must have an audible breath gap — flush
        // back-to-back rap turns read as mush even with zero overlap.
        let a = continuous_source("a", 200.0, 12.0);
        let b = continuous_source("b", 800.0, 12.0);
        let cfg = PingpongConfig {
            vocals: vec!["a".into(), "b".into()],
            phrase_beats: 4.0,
            loop_prob: 0.0,
            pitch_prob: 0.0,
            seed: 5,
            start_beat: 0.0,
            end_beat: None,
            gain_db: 0.0,
            gap_ms: 150.0,
            voice_pan: 0.0,
            voice_tilt_db: 0.0,
            throw_prob: 0.0,
        };
        let placed = build_pingpong(&cfg, &[a, b], 120.0, SR).unwrap();
        let mut spans: Vec<(usize, usize)> =
            placed.iter().map(|p| (p.offset, p.offset + p.l.len())).collect();
        spans.sort_by_key(|s| s.0);
        let min_gap = (0.12 * SR as f64) as i64; // ≥120 ms of the 150 ms breath
        for w in spans.windows(2) {
            let gap = w[1].0 as i64 - w[0].1 as i64;
            assert!(
                gap >= min_gap,
                "breath gap too small between turns: {} ms",
                gap as f64 / SR as f64 * 1000.0
            );
        }
    }

    #[test]
    fn pingpong_stops_at_end_beat() {
        // end_beat caps the dialogue so it can't outlive the beat into an
        // a-cappella tail over silence.
        let a = continuous_source("a", 200.0, 12.0);
        let b = continuous_source("b", 800.0, 12.0);
        let cfg = PingpongConfig {
            vocals: vec!["a".into(), "b".into()],
            phrase_beats: 4.0,
            loop_prob: 0.0,
            pitch_prob: 0.0,
            seed: 5,
            start_beat: 0.0,
            end_beat: Some(8.0),
            gain_db: 0.0,
            gap_ms: 150.0,
            voice_pan: 0.0,
            voice_tilt_db: 0.0,
            throw_prob: 0.0,
        };
        let placed = build_pingpong(&cfg, &[a, b], 120.0, SR).unwrap();
        assert_eq!(placed.len(), 2, "8 beats / 4-beat slots = 2 phrases");
        let end = beat_to_samples(8.0, 120.0, SR);
        for p in &placed {
            assert!(p.offset + p.l.len() <= end, "phrase must end before end_beat");
        }
    }

    #[test]
    fn pingpong_splits_voices_in_stereo() {
        // voice_pan must push voice A left and voice B right so similar rap
        // timbres stay trackable by ear position.
        let a = continuous_source("a", 200.0, 8.0);
        let b = continuous_source("b", 900.0, 8.0);
        let cfg = PingpongConfig {
            vocals: vec!["a".into(), "b".into()],
            phrase_beats: 4.0,
            loop_prob: 0.0,
            pitch_prob: 0.0,
            seed: 5,
            start_beat: 0.0,
            end_beat: None,
            gain_db: 0.0,
            gap_ms: 150.0,
            voice_pan: 0.2,
            voice_tilt_db: 0.0,
            throw_prob: 0.0,
        };
        let placed = build_pingpong(&cfg, &[a, b], 120.0, SR).unwrap();
        let rms = |x: &[f32]| {
            (x.iter().map(|v| (*v as f64) * (*v as f64)).sum::<f64>() / x.len() as f64).sqrt()
        };
        let is_high = |p: &PlacedPhrase| {
            let zc = p.l.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
            (zc as f64 / (p.l.len() as f64 / SR as f64)) > 450.0
        };
        for p in &placed {
            let (rl, rr) = (rms(&p.l), rms(&p.r));
            if is_high(p) {
                assert!(rr > rl * 1.1, "voice B should sit right: L {rl:.4} R {rr:.4}");
            } else {
                assert!(rl > rr * 1.1, "voice A should sit left: L {rl:.4} R {rr:.4}");
            }
        }
    }
}
