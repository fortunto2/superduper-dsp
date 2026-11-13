//! `sdsp-chain verify`: a mix change stays only if two independent instruments agree.
//!
//! Port of tools/mixverify.py. Instrument 1 is the offline render (this binary renders both TOMLs and
//! profiles them with `check`); instrument 2 is the bus log (`mix_masking` runs in mix-log.jsonl). A change
//! is KEEP only when both move the same way by at least their threshold and no guard trips. One instrument
//! is never an argument. Exit code: KEEP 0, UNDO 1, UNKNOWN 2.

use std::path::{Path, PathBuf};
use std::process::Command;

use realfft::RealFftPlanner;
use serde_json::{json, Map, Value};

use crate::check::{self, BANDS};

pub const LEVEL_DB: f64 = 1.0;
pub const BAND_DB: f64 = 1.5;
pub const SHARE: f64 = 0.10;
const N_FFT: usize = 4096;

pub const USAGE: &str = "Usage: sdsp-chain verify <before.toml> <after.toml> [--question masking|balance|loudness|mono]
                           [--labels before,after] [--section A B] [--ref ref.wav --ref-start S --ref-end E]
                           [--pair A,B] [--band LO HI] [--track NAME] [--expect up|down]
                           [--input in.wav] [--mix-log FILE] [--chain BIN] [--keep DIR] [--json]
Exit code: KEEP 0, UNDO 1, UNKNOWN 2. Needs both instruments: the offline render and two labelled bus runs.";

pub struct Args {
    pub before: String,
    pub after: String,
    pub question: String,
    pub labels: Option<String>,
    pub section: Option<(f64, f64)>,
    pub reference: Option<String>,
    pub ref_span: (Option<f64>, Option<f64>),
    pub pair: Option<String>,
    pub band: Option<(f64, f64)>,
    pub track: Option<String>,
    pub expect: Option<String>,
    pub input: Option<String>,
    pub mix_log: PathBuf,
    pub chain: Option<String>,
    pub keep: Option<String>,
    pub json: bool,
}

pub fn parse_args(args: &[String]) -> Result<Args, String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut a = Args {
        before: String::new(),
        after: String::new(),
        question: "masking".into(),
        labels: None,
        section: None,
        reference: None,
        ref_span: (None, None),
        pair: None,
        band: None,
        track: None,
        expect: None,
        input: None,
        mix_log: Path::new(&home).join(".superduper-dsp/mix-log.jsonl"),
        chain: None,
        keep: None,
        json: false,
    };
    let mut it = args.iter();
    let mut pos = vec![];
    let text = |v: Option<&String>, k: &str| -> Result<String, String> { v.cloned().ok_or(format!("{k} needs a value")) };
    let num = |v: Option<&String>, k: &str| -> Result<f64, String> { text(v, k)?.parse::<f64>().map_err(|_| format!("{k}: not a number")) };
    while let Some(k) = it.next() {
        match k.as_str() {
            "--question" => {
                a.question = text(it.next(), k)?;
                if !["masking", "balance", "loudness", "mono"].contains(&a.question.as_str()) {
                    return Err(format!("--question: masking, balance, loudness or mono, not {:?}", a.question));
                }
            }
            "--labels" => a.labels = Some(text(it.next(), k)?),
            "--section" => a.section = Some((num(it.next(), k)?, num(it.next(), k)?)),
            "--ref" => a.reference = Some(text(it.next(), k)?),
            "--ref-start" => a.ref_span.0 = Some(num(it.next(), k)?),
            "--ref-end" => a.ref_span.1 = Some(num(it.next(), k)?),
            "--pair" => a.pair = Some(text(it.next(), k)?),
            "--band" => a.band = Some((num(it.next(), k)?, num(it.next(), k)?)),
            "--track" => a.track = Some(text(it.next(), k)?),
            "--expect" => a.expect = Some(text(it.next(), k)?),
            "--input" => a.input = Some(text(it.next(), k)?),
            "--mix-log" => a.mix_log = PathBuf::from(text(it.next(), k)?),
            "--chain" => a.chain = Some(text(it.next(), k)?),
            "--keep" => a.keep = Some(text(it.next(), k)?),
            "--json" => a.json = true,
            "-h" | "--help" => return Err(USAGE.into()),
            x if !x.starts_with("--") => pos.push(x.to_string()),
            x => return Err(format!("unexpected argument {x:?}\n{USAGE}")),
        }
    }
    if pos.len() != 2 {
        return Err(USAGE.into());
    }
    a.after = pos.pop().unwrap_or_default();
    a.before = pos.pop().unwrap_or_default();
    Ok(a)
}

// ---------------------------------------------------------------------------
// TOML text surgery: stems
// ---------------------------------------------------------------------------

fn header_kind(line: &str) -> Option<&'static str> {
    let t = line.split('#').next().unwrap_or("").trim();
    let inner = t.strip_prefix("[[")?.strip_suffix("]]")?.trim();
    match inner {
        "track" => Some("track"),
        "master" => Some("master"),
        _ => None,
    }
}

type Block = Vec<String>;

/// (preamble, track blocks, master blocks) as lines. A block runs from its `[[track]]`/`[[master]]` header
/// to the next one; the `[[track.stage]]` sub-tables stay inside their track.
pub fn split_toml(text: &str) -> (Block, Vec<Block>, Vec<Block>) {
    let (mut pre, mut tracks, mut masters) = (vec![], vec![], vec![]);
    let mut cur: Option<Block> = None;
    let mut kind = "track";
    for line in text.lines() {
        if let Some(k) = header_kind(line) {
            if let Some(c) = cur.take() {
                if kind == "track" { tracks.push(c) } else { masters.push(c) }
            }
            cur = Some(vec![line.to_string()]);
            kind = k;
        } else if let Some(c) = cur.as_mut() {
            c.push(line.to_string());
        } else {
            pre.push(line.to_string());
        }
    }
    if let Some(c) = cur {
        if kind == "track" { tracks.push(c) } else { masters.push(c) }
    }
    (pre, tracks, masters)
}

fn is_mute_line(l: &str) -> bool {
    l.trim_start().strip_prefix("mute").is_some_and(|r| r.trim_start().starts_with('='))
}

/// The same mix with every track but `keep` muted and no master chain: what that one part contributes.
/// Muted tracks still render as keys, so ducks and sidechains survive.
pub fn stem_toml(text: &str, keep: usize) -> String {
    let (pre, tracks, _) = split_toml(text);
    let mut out = pre;
    for (i, block) in tracks.iter().enumerate() {
        out.push(block[0].clone());
        if i != keep {
            out.push("mute = true".into());
        }
        out.extend(block[1..].iter().filter(|l| !is_mute_line(l)).cloned());
    }
    out.join("\n") + "\n"
}

fn track_names(cfg: &toml::Value) -> Vec<String> {
    let tracks = cfg.get("track").and_then(|t| t.as_array()).cloned().unwrap_or_default();
    tracks
        .iter()
        .enumerate()
        .map(|(i, t)| t.get("name").and_then(|n| n.as_str()).filter(|n| !n.is_empty()).map(String::from).unwrap_or(format!("track {}", i + 1)))
        .collect()
}

// ---------------------------------------------------------------------------
// Instrument 1: render and measure
// ---------------------------------------------------------------------------

/// The renderer: --chain, $SDSP_CHAIN, then this very binary.
fn find_chain(explicit: Option<&str>) -> Option<PathBuf> {
    let env = std::env::var("SDSP_CHAIN").ok();
    let me = std::env::current_exe().ok();
    [explicit.map(String::from), env].into_iter().flatten().map(PathBuf::from).chain(me).find(|p| p.is_file())
}

fn tail(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    s.chars().rev().take(800).collect::<Vec<_>>().into_iter().rev().collect()
}

fn render(chain: &Path, toml_path: &Path, out_wav: &Path, text: Option<&str>, cli_input: Option<&str>) -> Result<(), String> {
    let toml_path = toml_path.canonicalize().map_err(|e| format!("{}: {e}", toml_path.display()))?;
    let cfg_file = match text {
        Some(t) => {
            let f = out_wav.with_extension("toml");
            std::fs::write(&f, t).map_err(|e| e.to_string())?;
            f
        }
        None => toml_path.clone(),
    };
    let cwd = toml_path.parent().unwrap_or(Path::new("."));
    let out = Command::new(chain).arg(&cfg_file).arg(cli_input.unwrap_or("unused.wav")).arg(out_wav).current_dir(cwd).output().map_err(|e| format!("sdsp-chain: {e}"))?;
    if !out.status.success() || !out_wav.is_file() {
        let name = cfg_file.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        return Err(format!("sdsp-chain failed on {name}:\n{}\n{}", tail(&out.stdout), tail(&out.stderr)));
    }
    Ok(())
}

/// Absolute energy (dB) of the mono fold in [lo, hi) Hz, mean per 4096-sample frame.
pub fn band_db(l: &[f64], r: &[f64], sr: f64, lo: f64, hi: f64) -> f64 {
    let mono: Vec<f64> = l.iter().zip(r).map(|(a, b)| (a + b) * 0.5).collect();
    let frames = mono.len() / N_FFT;
    if frames == 0 {
        return -120.0;
    }
    let win = check::hann(N_FFT);
    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(N_FFT);
    let (mut buf, mut spec) = (fft.make_input_vec(), fft.make_output_vec());
    let hz = |k: usize| k as f64 * sr / N_FFT as f64;
    let mut total = 0.0;
    for f in 0..frames {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = mono[f * N_FFT + i] * win[i];
        }
        if fft.process(&mut buf, &mut spec).is_err() {
            return -120.0;
        }
        total += spec.iter().enumerate().filter(|(k, _)| hz(*k) >= lo && hz(*k) < hi).map(|(_, c)| c.norm_sqr()).sum::<f64>();
    }
    10.0 * (total / frames as f64 + 1e-20).log10()
}

type Span = (Option<f64>, Option<f64>);

fn stem_reading(path: &Path, section: Span, band: Option<(f64, f64)>) -> Result<Value, String> {
    let a = check::load(path, section.0, section.1)?;
    let n = a.l.len().min(a.r.len());
    let mono_sq: f64 = a.l[..n].iter().zip(&a.r[..n]).map(|(x, y)| ((x + y) * 0.5).powi(2)).sum::<f64>() / n as f64;
    let mut out = json!({"rms_db": 10.0 * (mono_sq + 1e-20).log10()});
    if let Some((lo, hi)) = band {
        out["band_db"] = json!(band_db(&a.l[..n], &a.r[..n], a.sr, lo, hi));
    }
    Ok(out)
}

struct Renderer<'a> {
    chain: &'a Path,
    tmp: &'a Path,
    section: Span,
    band: Option<(f64, f64)>,
    cli_input: Option<String>,
}

impl Renderer<'_> {
    /// Instrument 1 for one TOML: the mix profile and the named stems.
    fn side(&self, toml_path: &str, tag: &str, stems: &[String]) -> Result<Value, String> {
        let text = std::fs::read_to_string(toml_path).map_err(|e| format!("{toml_path}: {e}"))?;
        let cfg: toml::Value = toml::from_str(&text).map_err(|e| format!("{toml_path}: {e}"))?;
        let names = track_names(&cfg);
        let first_input = cfg.get("track").and_then(|t| t.as_array()).into_iter().flatten().find_map(|t| t.get("input").and_then(|i| i.as_str())).map(String::from);
        let placeholder = self.cli_input.clone().or(first_input);
        let ph = placeholder.as_deref();
        let mix_wav = self.tmp.join(format!("{tag}-mix.wav"));
        render(self.chain, Path::new(toml_path), &mix_wav, None, ph)?;
        let mix = check::profile(&mix_wav, self.section.0, self.section.1)?;
        let mut stem_map = Map::new();
        for name in stems {
            let Some(i) = names.iter().position(|n| n == name) else { continue };
            let wav = self.tmp.join(format!("{tag}-stem{i}.wav"));
            render(self.chain, Path::new(toml_path), &wav, Some(&stem_toml(&text, i)), ph)?;
            stem_map.insert(name.clone(), stem_reading(&wav, self.section, self.band)?);
        }
        Ok(json!({"toml": toml_path, "tracks": names, "mix": mix, "stems": stem_map}))
    }
}

// ---------------------------------------------------------------------------
// Instrument 2: the bus log
// ---------------------------------------------------------------------------

/// The latest run with this label in mix-log.jsonl.
fn load_run(log: &Path, label: &str) -> Option<Value> {
    let text = std::fs::read_to_string(log).ok()?;
    text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).rfind(|r| r["label"].as_str() == Some(label))
}

pub(crate) type ClashKey = (String, String, i64, i64);

pub(crate) fn clash_key(c: &Value) -> ClashKey {
    let (mut a, mut b) = (c["a"].as_str().unwrap_or("").to_string(), c["b"].as_str().unwrap_or("").to_string());
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    (a, b, py_round(c["low_hz"].as_f64().unwrap_or(f64::NAN)), py_round(c["high_hz"].as_f64().unwrap_or(f64::NAN)))
}

/// Python's round(): half to even.
fn py_round(x: f64) -> i64 {
    if !x.is_finite() {
        return i64::MIN;
    }
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 && r % 2.0 != 0.0 { (r - x.signum()) as i64 } else { r as i64 }
}

pub(crate) fn pair_name(c: &Value) -> String {
    format!("{} × {}", c["a"].as_str().unwrap_or(""), c["b"].as_str().unwrap_or(""))
}

pub(crate) type Levels = Vec<(String, f64)>;

pub(crate) fn levels_of(run: &Value) -> Levels {
    run["levels"].as_array().into_iter().flatten().filter_map(|x| Some((x["track"].as_str().filter(|t| !t.is_empty())?.to_string(), x["rms_db"].as_f64()?))).collect()
}

pub(crate) fn level(levels: &Levels, name: &str) -> Option<f64> {
    levels.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
}

pub(crate) type BuriedKey = (String, String, i64);

pub(crate) fn buried_key(b: &Value) -> BuriedKey {
    (b["quiet"].as_str().unwrap_or("").into(), b["under"].as_str().unwrap_or("").into(), py_round(b["low_hz"].as_f64().unwrap_or(f64::NAN)))
}

pub(crate) fn buried_share(run: &Value, key: &BuriedKey) -> f64 {
    run["buried"].as_array().into_iter().flatten().find(|x| &buried_key(x) == key).and_then(|x| x["share"].as_f64()).unwrap_or(0.0)
}

/// The one candidate equal to, containing or contained in `query` (case-insensitive); None when absent or ambiguous.
pub fn match_name(query: &str, candidates: &[String]) -> Option<String> {
    let q = query.to_lowercase();
    let q = q.trim();
    let exact: Vec<_> = candidates.iter().filter(|c| c.to_lowercase().trim() == q).collect();
    if exact.len() == 1 {
        return Some(exact[0].clone());
    }
    let loose: Vec<_> = candidates.iter().filter(|c| c.to_lowercase().contains(q) || q.contains(&c.to_lowercase())).collect();
    if loose.len() == 1 { Some(loose[0].clone()) } else { None }
}

// ---------------------------------------------------------------------------
// The questions
// ---------------------------------------------------------------------------

/// A signed movement: positive means "the thing asked about went up".
pub fn reading(before: f64, after: f64, threshold: f64, unit: &str, what: &str) -> Value {
    let delta = after - before;
    let mv = if delta >= threshold { 1 } else if delta <= -threshold { -1 } else { 0 };
    json!({"what": what, "before": before, "after": after, "delta": delta, "threshold": threshold, "unit": unit, "move": mv})
}

pub fn missing(reason: &str) -> Value {
    json!({"missing": reason})
}

struct Ctx<'a> {
    r1: Option<&'a Value>,
    r2: Option<&'a Value>,
    b1: Option<&'a Value>,
    b2: Option<&'a Value>,
}

impl Ctx<'_> {
    fn renders(&self) -> Option<(&Value, &Value)> {
        Some((self.r1?, self.r2?))
    }
    fn bus(&self) -> Option<(&Value, &Value)> {
        Some((self.b1?, self.b2?))
    }
}

fn by_share(x: &Value, y: &Value) -> std::cmp::Ordering {
    x["share"].as_f64().partial_cmp(&y["share"].as_f64()).unwrap_or(std::cmp::Ordering::Equal)
}

/// The clash named by --pair that the before run knows (so its band comes with it).
fn known_clash(bb: &Value, a: &str, b: &str) -> Option<Value> {
    let mut sorted: Vec<Value> = bb["clashes"].as_array().cloned().unwrap_or_default();
    sorted.sort_by(|x, y| by_share(y, x));
    for c in sorted {
        let (ca, cb) = (c["a"].as_str().unwrap_or("").to_string(), c["b"].as_str().unwrap_or("").to_string());
        let mut names: Vec<String> = bb["tracks"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(String::from)).collect();
        names.push(ca.clone());
        names.push(cb.clone());
        if let (Some(x), Some(y)) = (match_name(a, &names), match_name(b, &names)) {
            if (x == ca && y == cb) || (x == cb && y == ca) {
                return Some(c);
            }
        }
    }
    None
}

/// The clash the question is about: --pair (with --band or the bus band), else the worst clash in the before run.
fn choose_pair(args: &Args, b_before: Option<&Value>) -> Option<Value> {
    if let Some(p) = &args.pair {
        let (a, b) = p.split_once(',').map(|(a, b)| (a.trim().to_string(), b.trim().to_string())).unwrap_or((p.trim().to_string(), String::new()));
        if let Some(c) = b_before.and_then(|bb| known_clash(bb, &a, &b)) {
            return Some(c);
        }
        return Some(match args.band {
            Some((lo, hi)) => json!({"a": a, "b": b, "low_hz": lo, "high_hz": hi, "share": 0.0}),
            None => json!({"a": a, "b": b, "low_hz": null, "high_hz": null, "share": 0.0}),
        });
    }
    b_before?["clashes"].as_array()?.iter().cloned().max_by(by_share)
}

fn masking_bus(cx: &Ctx, pair: &Value) -> Value {
    let share = |run: &Value| run["clashes"].as_array().into_iter().flatten().find(|c| clash_key(c) == clash_key(pair)).and_then(|c| c["share"].as_f64()).unwrap_or(0.0);
    let Some((b1, b2)) = cx.bus() else { return missing("bus runs not loaded") };
    let (s1, s2) = (share(b1), share(b2));
    let mut r = reading(-s1, -s2, SHARE, "share", &format!("clash share {}", pair_name(pair)));
    r["before"] = json!(s1);
    r["after"] = json!(s2);
    r["delta"] = json!(s2 - s1);
    r
}

fn ask_masking(cx: &Ctx, pair: Option<&Value>) -> (Value, Value) {
    let Some(pair) = pair else { return (missing("no clash pair: no clashes in the before run and no --pair"), missing("no clash pair to read")) };
    let bus = masking_bus(cx, pair);
    let (Some(lo), Some(hi)) = (pair["low_hz"].as_f64(), pair["high_hz"].as_f64()) else {
        return (missing("no band for the pair (give --band LO HI or --labels)"), bus);
    };
    let Some((r1, r2)) = cx.renders() else { return (missing("renders not available"), bus) };
    let (a, b) = (r1["stem_names"][pair["a"].as_str().unwrap_or("")].as_str(), r1["stem_names"][pair["b"].as_str().unwrap_or("")].as_str());
    let (Some(a), Some(b)) = (a, b) else {
        return (missing(&format!("no stem for {}: TOML tracks are {}", pair_name(pair), py_list(&r1["tracks"]))), bus);
    };
    let sep = |r: &Value| (r["stems"][a]["band_db"].as_f64().unwrap_or(f64::NAN) - r["stems"][b]["band_db"].as_f64().unwrap_or(f64::NAN)).abs();
    (reading(sep(r1), sep(r2), BAND_DB, "dB", &format!("separation {a} / {b} at {lo:.0}-{hi:.0} Hz")), bus)
}

/// A JSON string list the way Python prints it: ['a', 'b'].
fn py_list(v: &Value) -> String {
    let items: Vec<String> = v.as_array().into_iter().flatten().map(|x| format!("'{}'", x.as_str().unwrap_or(""))).collect();
    format!("[{}]", items.join(", "))
}

fn ask_balance(cx: &Ctx, track: Option<&str>) -> (Value, Value) {
    let Some(track) = track else { return (missing("no track: give --track or --labels"), missing("no track")) };
    let bus = match cx.bus() {
        None => missing("bus runs not loaded"),
        Some((b1, b2)) => {
            let (l1, l2) = (levels_of(b1), levels_of(b2));
            let all: Vec<String> = l1.iter().chain(&l2).map(|(n, _)| n.clone()).collect();
            match match_name(track, &all).and_then(|t| Some((level(&l1, &t)?, level(&l2, &t)?, t))) {
                Some((a, b, t)) => reading(a, b, LEVEL_DB, "dB", &format!("bus level {t}")),
                None => missing(&format!("track '{track}' not in both runs")),
            }
        }
    };
    let Some((r1, r2)) = cx.renders() else { return (missing("renders not available"), bus) };
    let Some(t) = r1["stem_names"][track].as_str() else {
        return (missing(&format!("no stem for '{track}': TOML tracks are {}", py_list(&r1["tracks"]))), bus);
    };
    let rms = |r: &Value| r["stems"][t]["rms_db"].as_f64().unwrap_or(f64::NAN);
    (reading(rms(r1), rms(r2), LEVEL_DB, "dB", &format!("stem RMS {t}")), bus)
}

fn ask_loudness(cx: &Ctx) -> (Value, Value) {
    let bus = match cx.bus() {
        None => missing("bus runs not loaded"),
        Some((b1, b2)) => match (level(&levels_of(b1), "Master"), level(&levels_of(b2), "Master")) {
            (Some(a), Some(b)) => reading(a, b, LEVEL_DB, "dB", "bus Master level"),
            _ => missing("no 'Master' level in both runs"),
        },
    };
    let Some((r1, r2)) = cx.renders() else { return (missing("renders not available"), bus) };
    match (r1["mix"]["lufs_integrated"].as_f64(), r2["mix"]["lufs_integrated"].as_f64()) {
        (Some(a), Some(b)) if a.is_finite() && b.is_finite() => (reading(a, b, LEVEL_DB, "LU", "integrated LUFS"), bus),
        _ => (missing("LUFS unreadable (silence?)"), bus),
    }
}

fn mono_values(mix: &Value) -> Vec<f64> {
    mix["mono_loss_db"].as_object().map(|m| m.values().filter_map(|v| v.as_f64()).collect()).unwrap_or_default()
}

fn ask_mono(cx: &Ctx) -> (Value, Value) {
    let bus = missing("the bus logs no mono measure (mix_phase is not written to mix-log.jsonl)");
    let Some((r1, r2)) = cx.renders() else { return (missing("renders not available"), bus) };
    let (m1, m2) = (mono_values(&r1["mix"]), mono_values(&r2["mix"]));
    if m1.is_empty() || m2.is_empty() {
        return (missing("mono loss unreadable"), bus);
    }
    let worst = |m: &[f64]| m.iter().cloned().fold(f64::INFINITY, f64::min);
    (reading(worst(&m1), worst(&m2), BAND_DB, "dB", "worst-band mono loss"), bus)
}

// ---------------------------------------------------------------------------
// Guards and verdict
// ---------------------------------------------------------------------------

pub(crate) type Tripped = Vec<(String, String)>;

pub(crate) fn bands_touching(band: Option<(f64, f64)>) -> Vec<&'static str> {
    match band {
        Some((lo, hi)) => BANDS.iter().filter(|(l, h, _)| *l < hi && *h > lo).map(|(_, _, n)| *n).collect(),
        None => vec![],
    }
}

pub(crate) fn pair_band(pair: Option<&Value>) -> Option<(f64, f64)> {
    let p = pair?;
    Some((p["low_hz"].as_f64()?, p["high_hz"].as_f64()?))
}

fn render_guards(question: &str, r1: &Value, r2: &Value, pair: Option<&Value>, out: &mut Tripped) {
    let (p1, p2) = (&r1["mix"], &r2["mix"]);
    let mut push = |t: String| out.push(("render".into(), t));
    if question == "masking" || question == "mono" {
        if let (Some(a), Some(b)) = (p1["lufs_integrated"].as_f64(), p2["lufs_integrated"].as_f64()) {
            if (b - a).abs() > LEVEL_DB {
                push(format!("integrated LUFS moved {:+.1} LU (> {})", b - a, py_float(LEVEL_DB)));
            }
        }
    }
    if question != "balance" {
        let exempt = if question == "masking" { bands_touching(pair_band(pair)) } else { vec![] };
        for (_, _, name) in BANDS.iter() {
            let (Some(a), Some(b)) = (p1["bands_db"][*name].as_f64(), p2["bands_db"][*name].as_f64()) else { continue };
            if !exempt.contains(name) && (b - a).abs() > BAND_DB {
                push(format!("band {name} moved {:+.1} dB (> {BAND_DB}) outside the question", b - a));
            }
        }
    }
    if question != "mono" {
        for (_, _, name) in BANDS.iter() {
            if let (Some(a), Some(b)) = (p1["mono_loss_db"][*name].as_f64(), p2["mono_loss_db"][*name].as_f64()) {
                if a - b > BAND_DB {
                    push(format!("mono loss in {name} worsened {:+.1} dB (> {BAND_DB})", b - a));
                }
            }
        }
    }
}

fn bus_guards(question: &str, b1: &Value, b2: &Value, pair: Option<&Value>, track: Option<&str>, out: &mut Tripped) {
    let c1: Vec<(ClashKey, f64)> = b1["clashes"].as_array().into_iter().flatten().map(|c| (clash_key(c), c["share"].as_f64().unwrap_or(0.0))).collect();
    for c in b2["clashes"].as_array().into_iter().flatten() {
        let k = clash_key(c);
        if pair.is_some_and(|p| clash_key(p) == k) {
            continue;
        }
        let before = c1.iter().find(|(kk, _)| *kk == k).map(|(_, s)| *s);
        let grew = c["share"].as_f64().unwrap_or(0.0) - before.unwrap_or(0.0);
        if grew >= SHARE {
            let verb = if before.is_some() { "grew" } else { "appeared" };
            let (lo, hi) = (c["low_hz"].as_f64().unwrap_or(0.0), c["high_hz"].as_f64().unwrap_or(0.0));
            out.push(("bus".into(), format!("clash {} at {lo:.0}-{hi:.0} Hz {verb} {:+.0} pt (≥ {:.0})", pair_name(c), grew * 100.0, SHARE * 100.0)));
        }
    }
    let Some(track) = track.filter(|_| question == "balance") else { return };
    let (l1, l2) = (levels_of(b1), levels_of(b2));
    let names: Vec<String> = l1.iter().map(|(n, _)| n.clone()).collect();
    let t = match_name(track, &names);
    for (name, a) in &l1 {
        let Some(b) = level(&l2, name) else { continue };
        if Some(name) != t.as_ref() && name != "Master" && (b - a).abs() > LEVEL_DB {
            out.push(("bus".into(), format!("level of {name} moved {:+.1} dB (> {}), not the track asked about", b - a, py_float(LEVEL_DB))));
        }
    }
}

/// Each tripped guard as (instrument, text). Exemptions are the metrics the question itself moves.
fn guards(question: &str, cx: &Ctx, pair: Option<&Value>, track: Option<&str>) -> Tripped {
    let mut out = vec![];
    if let Some((r1, r2)) = cx.renders() {
        render_guards(question, r1, r2, pair, &mut out);
    }
    if let Some((b1, b2)) = cx.bus() {
        bus_guards(question, b1, b2, pair, track, &mut out);
    }
    out
}

/// Python's str(float): 1.0 prints "1.0", 1.5 prints "1.5".
fn py_float(x: f64) -> String {
    if x == x.trunc() { format!("{x:.1}") } else { format!("{x}") }
}

fn describe(inst: &str, r: &Value) -> String {
    let (d, t) = (r["delta"].as_f64().unwrap_or(0.0), r["threshold"].as_f64().unwrap_or(0.0));
    let (what, unit) = (r["what"].as_str().unwrap_or(""), r["unit"].as_str().unwrap_or(""));
    if unit == "share" {
        format!("{inst}: {what} {:+.0} pt (threshold {:.0} pt)", d * 100.0, t * 100.0)
    } else {
        format!("{inst}: {what} {d:+.1} {unit} (threshold {} {unit})", py_float(t))
    }
}

/// The rule, in one place.
pub fn verdict(question: &str, rend: &Value, bus: &Value, tripped: &Tripped, expect: Option<&str>) -> (&'static str, Vec<String>) {
    for (inst, r) in [("render", rend), ("bus", bus)] {
        if let Some(m) = r.get("missing") {
            return ("UNKNOWN", vec![format!("{inst} instrument missing: {}; one instrument is not an argument", m.as_str().unwrap_or(""))]);
        }
    }
    let mut reasons = vec![describe("render", rend), describe("bus", bus)];
    let (m1, m2) = (rend["move"].as_i64().unwrap_or(0), bus["move"].as_i64().unwrap_or(0));
    if !tripped.is_empty() {
        reasons.extend(tripped.iter().map(|(i, t)| format!("guard: {i}: {t}")));
        return ("UNDO", reasons);
    }
    if m1 == 0 && m2 == 0 {
        reasons.push("no change on either instrument (both under threshold)".into());
        return ("UNKNOWN", reasons);
    }
    if m1 != m2 {
        reasons.push("the instruments disagree".into());
        return ("UNDO", reasons);
    }
    if question == "masking" || question == "mono" {
        let (v, why) = if m1 > 0 { ("KEEP", "both say better") } else { ("UNDO", "both say worse") };
        reasons.push(why.into());
        return (v, reasons);
    }
    let dir = if m1 > 0 { "up" } else { "down" };
    let want = match expect {
        Some("up") => Some(1),
        Some("down") => Some(-1),
        _ => None,
    };
    if want.is_some_and(|w| w != m1) {
        reasons.push(format!("both moved {dir}, you expected {}", expect.unwrap_or("")));
        return ("UNDO", reasons);
    }
    reasons.push(format!("both moved {dir}"));
    ("KEEP", reasons)
}

// ---------------------------------------------------------------------------
// run: everything but printing
// ---------------------------------------------------------------------------

struct Loaded {
    labels: Option<Vec<String>>,
    b1: Option<Value>,
    b2: Option<Value>,
    bus_note: Option<String>,
}

fn load_bus(args: &Args) -> Loaded {
    let labels: Option<Vec<String>> = args.labels.as_ref().map(|l| l.splitn(2, ',').map(|s| s.trim().to_string()).collect());
    let mut out = Loaded { labels: labels.clone(), b1: None, b2: None, bus_note: Some("no --labels".into()) };
    if let Some(l) = labels.filter(|l| l.len() == 2) {
        out.b1 = load_run(&args.mix_log, &l[0]);
        out.b2 = load_run(&args.mix_log, &l[1]);
        out.bus_note = if out.b1.is_some() && out.b2.is_some() {
            None
        } else {
            let gone: Vec<&str> = l.iter().zip([&out.b1, &out.b2]).filter(|(_, r)| r.is_none()).map(|(n, _)| n.as_str()).collect();
            Some(format!("label(s) not in {}: {}", args.mix_log.display(), gone.join(", ")))
        };
    }
    out
}

/// The track a balance question is about: --track, else the bus track that moved most.
fn pick_track(args: &Args, bus: &Loaded) -> Option<String> {
    if args.question != "balance" || args.track.is_some() {
        return args.track.clone();
    }
    let (l1, l2) = (levels_of(bus.b1.as_ref()?), levels_of(bus.b2.as_ref()?));
    l1.iter()
        .filter_map(|(n, a)| Some((n, (level(&l2, n)? - a).abs(), a.max(level(&l2, n)?))))
        .filter(|(n, _, hi)| *n != "Master" && *hi > -100.0)
        .max_by(|x, y| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(n, _, _)| n.clone())
}

/// Which tracks need a stem render: query name -> TOML track name.
fn wanted_stems(args: &Args, pair: Option<&Value>, track: Option<&str>) -> Result<Vec<(String, String)>, String> {
    let text = std::fs::read_to_string(&args.before).map_err(|e| format!("{}: {e}", args.before))?;
    let names = track_names(&toml::from_str::<toml::Value>(&text).map_err(|e| format!("{}: {e}", args.before))?);
    let queries: Vec<String> = match (args.question.as_str(), pair, track) {
        ("masking", Some(p), _) => vec![p["a"].as_str().unwrap_or("").into(), p["b"].as_str().unwrap_or("").into()],
        ("balance", _, Some(t)) => vec![t.to_string()],
        _ => vec![],
    };
    Ok(queries.into_iter().filter_map(|q| match_name(&q, &names).map(|m| (q, m))).collect())
}

struct RenderOut {
    r1: Option<Value>,
    r2: Option<Value>,
    note: Option<String>,
}

fn render_both(args: &Args, pair: Option<&Value>, track: Option<&str>) -> Result<RenderOut, String> {
    let Some(chain) = find_chain(args.chain.as_deref()) else {
        return Ok(RenderOut { r1: None, r2: None, note: Some("sdsp-chain not found: pass --chain or set SDSP_CHAIN".into()) });
    };
    let wanted = wanted_stems(args, pair, track)?;
    let stems: Vec<String> = wanted.iter().map(|(_, m)| m.clone()).collect();
    let tmp = match &args.keep {
        Some(d) => PathBuf::from(d),
        None => std::env::temp_dir().join(format!("sdsp-verify-{}", std::process::id())),
    };
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let rd = Renderer { chain: &chain, tmp: &tmp, section: args.section.map_or((None, None), |(a, b)| (Some(a), Some(b))), band: pair_band(pair), cli_input: args.input.clone() };
    let result = rd.side(&args.before, "before", &stems).and_then(|a| Ok((a, rd.side(&args.after, "after", &stems)?)));
    if args.keep.is_none() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    Ok(match result {
        Ok((mut a, mut b)) => {
            let map: Map<String, Value> = wanted.into_iter().map(|(q, m)| (q, json!(m))).collect();
            a["stem_names"] = Value::Object(map.clone());
            b["stem_names"] = Value::Object(map);
            RenderOut { r1: Some(a), r2: Some(b), note: None }
        }
        Err(e) => RenderOut { r1: None, r2: None, note: Some(e) },
    })
}

/// Everything but printing: the result document (also the --json).
pub fn run_verify(args: &Args) -> Result<Value, String> {
    let bus = load_bus(args);
    let pair = if args.question == "masking" { choose_pair(args, bus.b1.as_ref()) } else { None };
    let track = pick_track(args, &bus);
    let rend_out = render_both(args, pair.as_ref(), track.as_deref())?;
    let reference = match &args.reference {
        Some(r) => Some(check::profile(Path::new(r), args.ref_span.0, args.ref_span.1)?),
        None => None,
    };
    let cx = Ctx { r1: rend_out.r1.as_ref(), r2: rend_out.r2.as_ref(), b1: bus.b1.as_ref(), b2: bus.b2.as_ref() };
    let (mut rend, mut busr) = match args.question.as_str() {
        "masking" => ask_masking(&cx, pair.as_ref()),
        "balance" => ask_balance(&cx, track.as_deref()),
        "loudness" => ask_loudness(&cx),
        _ => ask_mono(&cx),
    };
    if let Some(n) = rend_out.note.as_deref().filter(|_| rend.get("missing").is_some()) {
        rend = missing(n);
    }
    if let Some(n) = bus.bus_note.as_deref().filter(|_| busr.get("missing").is_some()) {
        busr = missing(n);
    }
    let tripped = guards(&args.question, &cx, pair.as_ref(), track.as_deref());
    let (v, reasons) = verdict(&args.question, &rend, &busr, &tripped, args.expect.as_deref());
    Ok(json!({
        "question": args.question, "before": args.before, "after": args.after, "labels": bus.labels,
        "section": args.section.map(|(a, b)| json!([a, b])).unwrap_or(json!([null, null])),
        "pair": pair, "track": track, "verdict": v, "reasons": reasons,
        "render": {"reading": rend, "before": rend_out.r1, "after": rend_out.r2, "note": rend_out.note},
        "bus": {"reading": busr, "before": bus.b1, "after": bus.b2, "note": bus.bus_note},
        "guards": tripped.iter().map(|(i, t)| json!({"instrument": i, "text": t})).collect::<Vec<_>>(),
        "thresholds": {"level_db": LEVEL_DB, "band_db": BAND_DB, "share": SHARE},
        "reference": reference,
    }))
}

pub fn run(args: &[String]) -> Result<(), String> {
    let a = parse_args(args)?;
    let res = run_verify(&a)?;
    if a.json {
        println!("{}", serde_json::to_string_pretty(&res).map_err(|e| e.to_string())?);
    } else {
        crate::verify_print::print_report(&a, &res);
    }
    let code = match res["verdict"].as_str() {
        Some("KEEP") => 0,
        Some("UNDO") => 1,
        _ => 2,
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    use super::*;

    const BEFORE: &str = "[[track]]\nname = \"bass\"\ninput = \"bass.wav\"\n\n[[track]]\nname = \"chord\"\ninput = \"chord.wav\"\n\n[[master]]\nplugin = \"eq\"\n";
    const CUT_300: &str = "  [[track.stage]]\n  plugin = \"eq\"\n  params = { \"Mid Freq\" = 300.0, \"Mid Gain\" = -6.0, \"Mid Q\" = 2.0 }\n";

    #[test]
    fn stem_toml_mutes_the_others_and_drops_the_master() {
        let text = BEFORE.replace("input = \"chord.wav\"\n", &format!("input = \"chord.wav\"\nmute = false\n{CUT_300}"));
        let stem = stem_toml(&text, 1);
        assert!(!stem.contains("[[master]]"));
        let blocks: Vec<&str> = stem.split("[[track]]").skip(1).collect();
        assert!(blocks[0].contains("mute = true") && !blocks[1].contains("mute"));
        assert!(blocks[1].contains("[[track.stage]]") && blocks[1].contains("\"Mid Gain\" = -6.0"));
    }

    fn ok(d: f64) -> Value {
        reading(0.0, d, 1.0, "dB", "x")
    }

    #[test]
    fn verdict_rule_table() {
        let none: Tripped = vec![];
        assert_eq!(verdict("masking", &ok(2.0), &ok(2.0), &none, None).0, "KEEP");
        assert_eq!(verdict("masking", &ok(-2.0), &ok(-2.0), &none, None).0, "UNDO");
        assert_eq!(verdict("masking", &ok(2.0), &ok(0.5), &none, None).0, "UNDO");
        assert_eq!(verdict("masking", &ok(0.0), &ok(0.0), &none, None).0, "UNKNOWN");
        assert_eq!(verdict("masking", &ok(2.0), &ok(2.0), &vec![("bus".into(), "x grew".into())], None).0, "UNDO");
        assert_eq!(verdict("masking", &missing("no log"), &ok(2.0), &none, None).0, "UNKNOWN");
        assert_eq!(verdict("balance", &ok(-2.0), &ok(-2.0), &none, None).0, "KEEP");
        assert_eq!(verdict("balance", &ok(-2.0), &ok(-2.0), &none, Some("up")).0, "UNDO");
        assert_eq!(verdict("loudness", &ok(2.0), &ok(-2.0), &none, None).0, "UNDO");
    }

    #[test]
    fn names_match_loosely_but_never_ambiguously() {
        let names = vec!["bass".to_string(), "chord pad".to_string(), "chord stab".to_string()];
        assert_eq!(match_name("BASS", &names).as_deref(), Some("bass"));
        assert_eq!(match_name("chord", &names), None);
        assert_eq!(match_name("pad", &names).as_deref(), Some("chord pad"));
    }

    #[test]
    fn python_rounding_is_half_to_even() {
        assert_eq!((py_round(0.5), py_round(1.5), py_round(2.5), py_round(-0.5)), (0, 2, 2, 0));
    }

    #[test]
    fn reading_moves_only_past_the_threshold() {
        assert_eq!(reading(0.0, 0.9, 1.0, "dB", "x")["move"], 0);
        assert_eq!(reading(0.0, 1.0, 1.0, "dB", "x")["move"], 1);
        assert_eq!(reading(0.0, -1.2, 1.0, "dB", "x")["move"], -1);
    }
}
