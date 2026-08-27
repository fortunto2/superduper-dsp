//! sdsp-tune — offline melody correction ("auto-Melodyne").
//!
//! Live autotune sees one frame at a time, so it drags every frame toward a
//! scale degree and flattens the singing along with the wrong notes. Offline,
//! the whole take is known: the file is cut into NOTES, each note's median is
//! moved onto its target, and the shape inside the note — vibrato, scoop,
//! release — is left exactly as sung. That is the difference you hear.
//!
//! The note list is written out as text and read back, so hand-editing works
//! before any GUI exists: change a `target`, re-run with `--apply`, and only
//! that note moves.
//!
//! ```text
//! sdsp-tune analyse voice.wav                 # notes + PNG, no audio written
//! sdsp-tune fix voice.wav out.wav             # analyse + correct to C major
//! sdsp-tune fix voice.wav out.wav --key A --scale minor --amount 0.8
//! sdsp-tune fix voice.wav out.wav --apply voice.notes.txt   # your edits
//! ```

use std::path::{Path, PathBuf};
use superduper_synth_core::melody::{
    self, CorrectOpts, Frame, Note, SegmentOpts,
};
use superduper_synth_core::pitch_engine::{Mode as EngineMode, PitchEngine};
use superduper_synth_core::psola::PitchParams;
use superduper_synth_core::swiftf0::SwiftF0Tracker;

const BLOCK: usize = 256;

/// Scale masks, bit 0 = root. Mirrors superduper-tune's `scale::SCALES`.
const SCALES: &[(&str, u16)] = &[
    ("chromatic", 0b1111_1111_1111),
    ("major", 0b1010_1101_0101),
    ("minor", 0b0101_1010_1101),
    ("dorian", 0b0110_1010_1101),
    ("mixolydian", 0b0110_1101_0101),
    ("harmonic-minor", 0b1001_1010_1101),
    ("major-penta", 0b0010_1001_0101),
    ("minor-penta", 0b0100_1010_1001),
];

struct Audio {
    l: Vec<f32>,
    r: Vec<f32>,
    sr: f32,
    stereo: bool,
}

fn read_wav(path: &Path) -> Result<Audio, String> {
    let mut rd = hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = rd.spec();
    let ch = spec.channels as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => rd.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            rd.samples::<i32>().map(|s| s.unwrap_or(0) as f32 * scale).collect()
        }
    };
    let mut l = Vec::with_capacity(raw.len() / ch);
    let mut r = Vec::with_capacity(raw.len() / ch);
    for f in raw.chunks(ch) {
        l.push(f[0]);
        r.push(if ch > 1 { f[1] } else { f[0] });
    }
    Ok(Audio { l, r, sr: spec.sample_rate as f32, stereo: ch > 1 })
}

fn write_wav(path: &Path, a: &Audio) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: if a.stereo { 2 } else { 1 },
        sample_rate: a.sr as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| format!("{}: {e}", path.display()))?;
    for i in 0..a.l.len() {
        w.write_sample(a.l[i]).map_err(|e| e.to_string())?;
        if a.stereo {
            w.write_sample(a.r[i]).map_err(|e| e.to_string())?;
        }
    }
    w.finalize().map_err(|e| e.to_string())
}

/// Analyse the take with SwiftF0 — offline, so we use the model that holds
/// the octave on breathy material rather than the cheap one.
fn analyse(a: &Audio) -> Vec<Frame> {
    let mut tr = SwiftF0Tracker::new(a.sr, 150.0);
    let mut out = Vec::new();
    for i in 0..a.l.len() {
        let m = (a.l[i] + a.r[i]) * 0.5;
        if tr.push(m) {
            out.push(Frame { t: i as f32 / a.sr, hz: tr.current_hz(), conf: tr.confidence() });
        }
    }
    out
}

/// Render the correction through [`PitchEngine`], following the per-frame
/// shift curve.
///
/// This used to hardcode the phase vocoder, for a good reason: corrections
/// here are tens of cents, so transparency at small α is the whole game, and
/// PSOLA left noise only a few dB under the harmonics on a smooth tone. That
/// reason is now the router's, not this file's — `Auto` measures the material
/// and picks, so a real vocal take gets PSOLA (and its independent formant
/// axis) while a synth or a breathy take still gets the vocoder.
///
/// `min_hz` is the take's own floor, which matters more offline than
/// anywhere: latency is free here, so there is no reason not to let the epoch
/// finder reach the lowest note actually sung.
fn render(a: &Audio, frames: &[Frame], curve: &[f32], formant_st: f32, min_hz: f32) -> Audio {
    let mut sh = PitchEngine::with_floor(a.sr, BLOCK, min_hz);
    sh.set_mode(EngineMode::Auto);
    sh.prime(1.0, 1.0);
    // The engine emits the past: the block it hands back now was written
    // `latency` samples ago. Feeding it the shift for the CURRENT input time
    // lands each note's correction on its NEIGHBOUR — on short notes that
    // cancels the correction outright (measured: median error unchanged).
    let lat_s = sh.latency_samples() as f32 / a.sr;
    let mut out_l = vec![0.0f32; a.l.len()];
    let mut out_r = vec![0.0f32; a.r.len()];
    let mut at = 0usize;
    while at < a.l.len() {
        let n = BLOCK.min(a.l.len() - at);
        let t = (at as f32 / a.sr - lat_s).max(0.0);
        // Nearest analysis frame for the moment being synthesised.
        let fi = frames.partition_point(|f| f.t < t).min(frames.len().saturating_sub(1));
        let p = PitchParams {
            pitch_st: curve.get(fi).copied().unwrap_or(0.0),
            formant_st,
            mix: 1.0,
            output_lin: 1.0,
            bypassed: false,
        };
        let mut tmp_r = vec![0.0f32; n];
        sh.process(
            &a.l[at..at + n],
            &a.r[at..at + n],
            &mut out_l[at..at + n],
            &mut tmp_r,
            &p,
        );
        out_r[at..at + n].copy_from_slice(&tmp_r);
        at += n;
    }
    Audio { l: out_l, r: out_r, sr: a.sr, stereo: a.stereo }
}

// ---------------------------------------------------------------------------
// Picture: pitch curve + notes + targets, so the edit is visible at a glance
// ---------------------------------------------------------------------------

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u8>, // RGB
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Self { w, h, px: vec![18; w * h * 3] }
    }
    fn set(&mut self, x: isize, y: isize, c: [u8; 3]) {
        if x < 0 || y < 0 || x as usize >= self.w || y as usize >= self.h {
            return;
        }
        let i = (y as usize * self.w + x as usize) * 3;
        self.px[i..i + 3].copy_from_slice(&c);
    }
    fn line(&mut self, x0: isize, y0: isize, x1: isize, y1: isize, c: [u8; 3]) {
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let (mut x, mut y, mut err) = (x0, y0, dx + dy);
        loop {
            self.set(x, y, c);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }
    fn rect(&mut self, x0: isize, y0: isize, x1: isize, y1: isize, c: [u8; 3]) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                self.set(x, y, c);
            }
        }
    }
    fn save(&self, path: &Path) -> Result<(), String> {
        let f = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(f), self.w as u32, self.h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .map_err(|e| e.to_string())?
            .write_image_data(&self.px)
            .map_err(|e| e.to_string())
    }
}

fn draw(path: &Path, frames: &[Frame], notes: &[Note], conf_gate: f32) -> Result<(), String> {
    let (w, h) = (1400usize, 520usize);
    let mut c = Canvas::new(w, h);
    let voiced: Vec<f32> = frames
        .iter()
        .filter(|f| f.conf >= conf_gate)
        .map(|f| melody::hz_to_st(f.hz))
        .collect();
    if voiced.is_empty() {
        return Err("nothing voiced to draw".into());
    }
    let lo = voiced.iter().cloned().fold(f32::MAX, f32::min) - 2.0;
    let hi = voiced.iter().cloned().fold(f32::MIN, f32::max) + 2.0;
    let dur = frames.last().unwrap().t.max(0.001);
    let xs = |t: f32| (t / dur * (w - 40) as f32) as isize + 20;
    let ys = |st: f32| (h as f32 - 30.0 - (st - lo) / (hi - lo) * (h - 60) as f32) as isize;

    // Semitone grid; the octave lines brighter.
    for m in (lo.ceil() as i32)..=(hi.floor() as i32) {
        let y = ys(m as f32);
        let shade = if m.rem_euclid(12) == 0 { 60 } else { 32 };
        for x in 20..(w - 20) as isize {
            c.set(x, y, [shade, shade, shade]);
        }
    }
    // Target bars (what the note is being moved to) then the sung curve on top.
    for n in notes {
        let (x0, x1) = (xs(n.start_s), xs(n.end_s));
        let y = ys(n.target_st);
        c.rect(x0, y - 3, x1.max(x0 + 1), y + 3, [40, 110, 70]);
        let ys_ = ys(n.sung_st);
        c.line(x0, ys_, x1, ys_, [150, 120, 40]);
    }
    let mut prev: Option<(isize, isize)> = None;
    for f in frames {
        if f.conf < conf_gate {
            prev = None;
            continue;
        }
        let p = (xs(f.t), ys(melody::hz_to_st(f.hz)));
        if let Some(q) = prev {
            c.line(q.0, q.1, p.0, p.1, [230, 230, 245]);
        }
        prev = Some(p);
    }
    c.save(path)
}

// ---------------------------------------------------------------------------

fn usage() {
    println!("Usage:");
    println!("  sdsp-tune analyse <in.wav> [opts]           notes + PNG only");
    println!("  sdsp-tune fix <in.wav> <out.wav> [opts]     analyse, correct, render");
    println!();
    println!("Options:");
    println!("  --key <C..B>          key root for scale snapping (default C)");
    println!("  --scale <name>        chromatic|major|minor|dorian|mixolydian|");
    println!("                        harmonic-minor|major-penta|minor-penta (default major)");
    println!("  --amount <0..1>       how much of each note's error to remove (default 1)");
    println!("  --flatten <0..1>      how much vibrato to straighten (default 0 = keep it)");
    println!("  --formant <st>        independent formant shift (default 0)");
    println!("  --split <cents>       pitch jump that starts a new note (default 80)");
    println!("  --min-dur <s>         drop notes shorter than this (default 0.06)");
    println!("  --apply <notes.txt>   use hand-edited targets from a previous run");
    println!("  --notes <path>        where to write the note list (default <in>.notes.txt)");
    println!("  --png <path>          where to write the picture (default <in>.notes.png)");
}

struct Opts {
    key: u8,
    mask: u16,
    scale_name: String,
    correct: CorrectOpts,
    seg: SegmentOpts,
    formant: f32,
    apply: Option<PathBuf>,
    notes_path: Option<PathBuf>,
    png_path: Option<PathBuf>,
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        key: 0,
        mask: SCALES[1].1,
        scale_name: "major".into(),
        correct: CorrectOpts::default(),
        seg: SegmentOpts::default(),
        formant: 0.0,
        apply: None,
        notes_path: None,
        png_path: None,
    };
    let mut i = 0;
    while i < args.len() {
        let need = |i: usize| -> Result<&String, String> {
            args.get(i + 1).ok_or_else(|| format!("{} needs a value", args[i]))
        };
        match args[i].as_str() {
            "--key" => {
                let v = need(i)?;
                o.key = melody::parse_pitch(&format!("{v}4"))
                    .ok_or_else(|| format!("bad key {v:?}"))? as u8
                    % 12;
                i += 2;
            }
            "--scale" => {
                let v = need(i)?;
                let (n, m) = SCALES
                    .iter()
                    .find(|(n, _)| n.eq_ignore_ascii_case(v))
                    .ok_or_else(|| format!("unknown scale {v:?}"))?;
                o.scale_name = n.to_string();
                o.mask = *m;
                i += 2;
            }
            "--amount" => { o.correct.amount = need(i)?.parse().map_err(|_| "bad --amount")?; i += 2; }
            "--flatten" => { o.correct.flatten = need(i)?.parse().map_err(|_| "bad --flatten")?; i += 2; }
            "--formant" => { o.formant = need(i)?.parse().map_err(|_| "bad --formant")?; i += 2; }
            "--split" => { o.seg.split_cents = need(i)?.parse().map_err(|_| "bad --split")?; i += 2; }
            "--min-dur" => { o.seg.min_dur_s = need(i)?.parse().map_err(|_| "bad --min-dur")?; i += 2; }
            "--apply" => { o.apply = Some(PathBuf::from(need(i)?)); i += 2; }
            "--notes" => { o.notes_path = Some(PathBuf::from(need(i)?)); i += 2; }
            "--png" => { o.png_path = Some(PathBuf::from(need(i)?)); i += 2; }
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(o)
}

fn with_suffix(p: &Path, suffix: &str) -> PathBuf {
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    p.with_file_name(format!("{stem}{suffix}"))
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cmd = argv.first().map(|s| s.as_str()).unwrap_or("");
    let (fix, in_path, out_path, rest) = match cmd {
        "analyse" | "analyze" => {
            let p = argv.get(1).ok_or("analyse needs an input wav")?;
            (false, PathBuf::from(p), None, argv[2..].to_vec())
        }
        "fix" => {
            let p = argv.get(1).ok_or("fix needs an input wav")?;
            let o = argv.get(2).ok_or("fix needs an output wav")?;
            (true, PathBuf::from(p), Some(PathBuf::from(o)), argv[3..].to_vec())
        }
        _ => {
            usage();
            return Ok(());
        }
    };
    let mut o = parse_opts(&rest)?;
    let audio = read_wav(&in_path)?;
    println!("{}: {:.2} s @ {} Hz", in_path.display(), audio.l.len() as f32 / audio.sr, audio.sr);

    let frames = analyse(&audio);
    let mut notes = melody::segment(&frames, &o.seg);
    melody::plan_targets(&mut notes, o.key, o.mask);
    if let Some(path) = &o.apply {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let n = melody::parse_notes(&text, &mut notes)?;
        println!("applied {n} hand-edited targets from {}", path.display());
        // The 3-semitone auto limit exists to stop a tracker octave error from
        // transposing a word. A target typed by hand is not a tracker error,
        // so it gets the full range.
        o.correct.max_shift_st = 24.0;
    }

    let voiced = frames.iter().filter(|f| f.conf >= o.seg.conf_gate).count();
    println!(
        "{} notes over {} voiced frames ({:.0}% of the take), key {} {}",
        notes.len(),
        voiced,
        voiced as f32 / frames.len().max(1) as f32 * 100.0,
        melody::NOTE_NAMES[o.key as usize],
        o.scale_name
    );
    if !notes.is_empty() {
        let mut errs: Vec<f32> = notes.iter().map(|n| n.error_cents().abs()).collect();
        errs.sort_by(f32::total_cmp);
        let med = errs[errs.len() / 2];
        let worst = errs[errs.len() - 1];
        let off = errs.iter().filter(|&&e| e > 20.0).count();
        println!(
            "intonation: median |error| {med:.0} cents, worst {worst:.0} cents, \
             {off} notes more than 20 cents off"
        );
    }

    let notes_path = o.notes_path.clone().unwrap_or_else(|| with_suffix(&in_path, ".notes.txt"));
    std::fs::write(&notes_path, melody::notes_to_text(&notes))
        .map_err(|e| format!("{}: {e}", notes_path.display()))?;
    println!("wrote {}", notes_path.display());

    let png_path = o.png_path.clone().unwrap_or_else(|| with_suffix(&in_path, ".notes.png"));
    match draw(&png_path, &frames, &notes, o.seg.conf_gate) {
        Ok(()) => println!("wrote {}", png_path.display()),
        Err(e) => println!("  (no picture: {e})"),
    }

    if !fix {
        println!("\nEdit the `target` column in the note list, then:");
        println!("  sdsp-tune fix {} out.wav --apply {}", in_path.display(), notes_path.display());
        return Ok(());
    }

    let curve = melody::shift_curve(&frames, &notes, &o.correct);
    let moved = curve.iter().filter(|c| c.abs() > 0.01).count();
    println!(
        "correcting: amount {:.2}, flatten {:.2} — {:.0}% of frames get a shift, max {:.2} st",
        o.correct.amount,
        o.correct.flatten,
        moved as f32 / curve.len().max(1) as f32 * 100.0,
        curve.iter().cloned().fold(0.0f32, |m, v| m.max(v.abs()))
    );
    // Track the real floor of the take so PSOLA's epoch finder can lock.
    let lowest = notes.iter().map(|n| n.sung_st).fold(f32::MAX, f32::min);
    let min_hz = if lowest.is_finite() {
        (melody::st_to_hz(lowest) * 0.85).clamp(50.0, 200.0)
    } else {
        95.0
    };
    println!("  engine: auto (PSOLA on a pulsed voice, phase vocoder otherwise), floor {min_hz:.0} Hz");
    let out = render(&audio, &frames, &curve, o.formant, min_hz);
    let out_path = out_path.unwrap();
    write_wav(&out_path, &out)?;
    println!("wrote {}", out_path.display());
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("sdsp-tune: {e}");
        std::process::exit(1);
    }
}
