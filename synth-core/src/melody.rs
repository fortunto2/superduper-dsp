//! Offline melody editing — a sung take as a list of NOTES, not a pitch curve.
//!
//! This is the model a Melodyne-style editor works on, and the reason offline
//! correction sounds different from live autotune. Live, the corrector sees
//! one frame at a time and drags each toward the nearest scale degree, so
//! vibrato and scoops are flattened along with the wrong intonation. Here the
//! whole take is known, so a note can be treated as one object: move its
//! **median** onto the target and leave the shape inside it alone. The singer
//! keeps their vibrato; only the note's centre moves.
//!
//! Pipeline: pitch curve (from any tracker) → [`segment`] → [`Note`]s →
//! [`plan_targets`] → per-frame semitone shift curve ([`shift_curve`]) that a
//! PSOLA shifter can follow. The note list is the editable surface: change a
//! `target_st`, re-render, and only that note moves.
//!
//! Not RT-safe and not meant to be — it allocates and needs the whole take.
//! Used by `tools/sdsp-tune`; a standalone editor would sit on the same model.

/// One frame of the analysed pitch curve.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub t: f32,
    /// Detected fundamental in Hz (meaningless when `conf` is low).
    pub hz: f32,
    /// Detector confidence, 0..1.
    pub conf: f32,
}

/// A sung note: a run of confident frames at one pitch.
#[derive(Clone, Debug)]
pub struct Note {
    pub start_s: f32,
    pub end_s: f32,
    /// Frame indices [first, last] in the analysed curve.
    pub first: usize,
    pub last: usize,
    /// Median pitch over the note, in MIDI semitones (fractional).
    pub sung_st: f32,
    /// Where the note should sit, in MIDI semitones. Whole numbers are
    /// in-tune notes; this is what an editor lets you drag.
    pub target_st: f32,
    /// Peak-to-peak pitch excursion inside the note, in cents — vibrato depth
    /// plus any scoop. Big values mean "expressive", not "out of tune".
    pub range_cents: f32,
}

impl Note {
    /// How far the note is out of tune, in cents (+ = sharp).
    pub fn error_cents(&self) -> f32 {
        (self.sung_st - self.target_st) * -100.0
    }
    pub fn dur_s(&self) -> f32 {
        self.end_s - self.start_s
    }
}

pub fn hz_to_st(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

pub fn st_to_hz(st: f32) -> f32 {
    440.0 * (2f32).powf((st - 69.0) / 12.0)
}

pub const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// "A4", "C#3" for a MIDI semitone.
pub fn note_name(st: f32) -> String {
    let m = st.round() as i32;
    let pc = m.rem_euclid(12) as usize;
    format!("{}{}", NOTE_NAMES[pc], m / 12 - 1)
}

#[derive(Clone, Copy, Debug)]
pub struct SegmentOpts {
    /// Frames below this confidence are unvoiced and break a note.
    pub conf_gate: f32,
    /// A sustained jump larger than this starts a new note.
    pub split_cents: f32,
    /// Notes shorter than this are dropped as glides / artefacts.
    pub min_dur_s: f32,
    /// Ignore this much of a note's head when measuring its pitch — scoops
    /// into a note are expression, not intonation, and they drag the median.
    pub head_skip: f32,
}

impl Default for SegmentOpts {
    fn default() -> Self {
        Self {
            conf_gate: 0.5,
            split_cents: 80.0,
            min_dur_s: 0.06,
            head_skip: 0.25,
        }
    }
}

fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

/// Cut the pitch curve into notes. Boundaries come from three signals, in
/// order of authority: an unvoiced gap, a sustained pitch jump, and the
/// minimum-duration floor that merges the leftovers back.
pub fn segment(frames: &[Frame], o: &SegmentOpts) -> Vec<Note> {
    let mut notes = Vec::new();
    let mut run: Vec<usize> = Vec::new();

    // A jump only counts once it HOLDS — otherwise every vibrato peak and
    // every tracker wobble would start a note.
    let hold = 3usize;
    let mut push_run = |run: &mut Vec<usize>, notes: &mut Vec<Note>| {
        if run.is_empty() {
            return;
        }
        let first = run[0];
        let last = *run.last().unwrap();
        let start_s = frames[first].t;
        let end_s = frames[last].t;
        let skip = ((run.len() as f32 * o.head_skip) as usize).min(run.len() - 1);
        let mut body: Vec<f32> = run[skip..].iter().map(|&i| hz_to_st(frames[i].hz)).collect();
        let mut all: Vec<f32> = run.iter().map(|&i| hz_to_st(frames[i].hz)).collect();
        all.sort_by(f32::total_cmp);
        let lo = all[(all.len() as f32 * 0.05) as usize];
        let hi = all[((all.len() - 1) as f32 * 0.95) as usize];
        let sung = median(&mut body);
        notes.push(Note {
            start_s,
            end_s,
            first,
            last,
            sung_st: sung,
            target_st: sung.round(),
            range_cents: (hi - lo) * 100.0,
        });
        run.clear();
    };

    let mut i = 0usize;
    while i < frames.len() {
        let f = frames[i];
        if f.conf < o.conf_gate || !f.hz.is_finite() || f.hz <= 0.0 {
            push_run(&mut run, &mut notes);
            i += 1;
            continue;
        }
        if let Some(&f0) = run.first() {
            let cur = hz_to_st(frames[f0].hz);
            let d = (hz_to_st(f.hz) - cur).abs() * 100.0;
            if d > o.split_cents {
                // Confirm the jump holds for `hold` frames before splitting.
                let held = (0..hold).all(|k| {
                    frames
                        .get(i + k)
                        .map(|g| {
                            g.conf >= o.conf_gate
                                && (hz_to_st(g.hz) - cur).abs() * 100.0 > o.split_cents * 0.6
                        })
                        .unwrap_or(false)
                });
                if held {
                    push_run(&mut run, &mut notes);
                }
            }
        }
        run.push(i);
        i += 1;
    }
    push_run(&mut run, &mut notes);
    notes.retain(|n| n.dur_s() >= o.min_dur_s);
    notes
}

/// 12-bit scale mask, bit 0 = root. Same convention as `superduper-tune`'s
/// `scale::SCALES`.
pub fn nearest_in_scale(st: f32, key: u8, mask: u16) -> f32 {
    if mask == 0 {
        return st.round();
    }
    let mut best = st.round();
    let mut best_d = f32::INFINITY;
    for cand in (st.round() as i32 - 2)..=(st.round() as i32 + 2) {
        let deg = (cand - key as i32).rem_euclid(12);
        if mask & (1 << deg) == 0 {
            continue;
        }
        let d = (cand as f32 - st).abs();
        if d < best_d {
            best_d = d;
            best = cand as f32;
        }
    }
    best
}

/// Set every note's target to the nearest allowed scale degree.
pub fn plan_targets(notes: &mut [Note], key: u8, mask: u16) {
    for n in notes.iter_mut() {
        n.target_st = nearest_in_scale(n.sung_st, key, mask);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CorrectOpts {
    /// How much of the note's error to remove, 0..1.
    pub amount: f32,
    /// How much of the *within-note* pitch movement to flatten, 0..1.
    /// 0 keeps vibrato and scoops exactly as sung — the reason to work
    /// offline at all. 1 gives a dead-straight note (hard-tune, offline).
    pub flatten: f32,
    /// Crossfade between adjacent notes' corrections, seconds. Stops a step
    /// in the shift curve at a note boundary from clicking.
    pub glide_s: f32,
    /// Never move a note further than this (a tracker error shouldn't
    /// transpose a word).
    pub max_shift_st: f32,
}

impl Default for CorrectOpts {
    fn default() -> Self {
        Self {
            amount: 1.0,
            flatten: 0.0,
            glide_s: 0.04,
            max_shift_st: 3.0,
        }
    }
}

/// Per-frame semitone shift to hand a pitch shifter, one value per input
/// frame. Frames outside any note get the neighbouring notes' correction
/// blended, so unvoiced gaps don't snap the shifter back to zero.
pub fn shift_curve(frames: &[Frame], notes: &[Note], o: &CorrectOpts) -> Vec<f32> {
    let mut curve = vec![0.0f32; frames.len()];
    if notes.is_empty() {
        return curve;
    }
    let dt = if frames.len() > 1 { frames[1].t - frames[0].t } else { 0.01 };
    let glide_frames = ((o.glide_s / dt.max(1e-6)) as usize).max(1);

    for n in notes {
        let base = ((n.target_st - n.sung_st) * o.amount).clamp(-o.max_shift_st, o.max_shift_st);
        for i in n.first..=n.last {
            // Flatten works on the deviation from the note's own median, so
            // `flatten = 0` leaves every wiggle intact.
            let dev = hz_to_st(frames[i].hz) - n.sung_st;
            curve[i] = base - dev * o.flatten;
        }
    }
    // Fill gaps by holding the previous note's correction, then smooth the
    // seams so the shifter never steps.
    let mut last = curve[notes[0].first];
    for i in 0..curve.len() {
        let inside = notes.iter().any(|n| i >= n.first && i <= n.last);
        if inside {
            last = curve[i];
        } else {
            curve[i] = last;
        }
    }
    let src = curve.clone();
    for i in 0..curve.len() {
        let a = i.saturating_sub(glide_frames);
        let b = (i + glide_frames).min(src.len() - 1);
        curve[i] = src[a..=b].iter().sum::<f32>() / (b - a + 1) as f32;
    }
    curve
}

/// One editable line per note: index, time, duration, sung pitch, target,
/// correction, vibrato range. Parsed back by [`parse_notes`], so this text
/// file IS the manual editing surface until a GUI exists.
pub fn notes_to_text(notes: &[Note]) -> String {
    let mut s = String::from(
        "# sdsp-tune note list — edit the `target` column and re-run with --apply\n\
         # target: a note name (A4, C#3) or a MIDI number; `-` leaves the note alone\n\
         #\n\
         #  idx     start      dur       sung        target     error   vibrato\n",
    );
    for (i, n) in notes.iter().enumerate() {
        s.push_str(&format!(
            "{:5}  {:8.3}  {:6.3}  {:>4} {:+6.0}c  {:>6}  {:+7.0}c  {:6.0}c\n",
            i,
            n.start_s,
            n.dur_s(),
            note_name(n.sung_st),
            (n.sung_st - n.sung_st.round()) * 100.0,
            note_name(n.target_st),
            n.error_cents(),
            n.range_cents,
        ));
    }
    s
}

/// Read a (possibly hand-edited) note list back and apply its targets to
/// `notes`, matched by index. Unknown/`-` targets leave the note untouched.
pub fn parse_notes(text: &str, notes: &mut [Note]) -> Result<usize, String> {
    let mut applied = 0;
    for (ln, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 6 {
            return Err(format!("line {}: expected at least 6 columns", ln + 1));
        }
        let idx: usize = cols[0]
            .parse()
            .map_err(|_| format!("line {}: bad index {:?}", ln + 1, cols[0]))?;
        let n = notes
            .get_mut(idx)
            .ok_or_else(|| format!("line {}: note {idx} does not exist", ln + 1))?;
        let tgt = cols[5];
        if tgt == "-" {
            n.target_st = n.sung_st; // explicit "don't touch"
            continue;
        }
        n.target_st = parse_pitch(tgt)
            .ok_or_else(|| format!("line {}: cannot read target {tgt:?}", ln + 1))?;
        applied += 1;
    }
    Ok(applied)
}

/// "A4" / "C#3" / "69" → MIDI semitones.
pub fn parse_pitch(s: &str) -> Option<f32> {
    if let Ok(v) = s.parse::<f32>() {
        return Some(v);
    }
    let b = s.as_bytes();
    let mut i = 0;
    let letter = b.first()?.to_ascii_uppercase();
    i += 1;
    let mut pc = match letter {
        b'C' => 0,
        b'D' => 2,
        b'E' => 4,
        b'F' => 5,
        b'G' => 7,
        b'A' => 9,
        b'B' => 11,
        _ => return None,
    };
    while i < b.len() && (b[i] == b'#' || b[i] == b'b') {
        pc += if b[i] == b'#' { 1 } else { -1 };
        i += 1;
    }
    let oct: i32 = s[i..].parse().ok()?;
    Some(((oct + 1) * 12 + pc) as f32)
}
