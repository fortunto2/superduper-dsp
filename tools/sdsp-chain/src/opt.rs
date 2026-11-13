//! `sdsp-chain opt`: move a few whitelisted plugin parameters until a measured loss stops falling.
//!
//! Port of tools/mixopt.py. The renderer (this binary) is deterministic and a render costs ~0.1 s, so the
//! mix is a function we can minimise directly: a bounded Nelder-Mead over the handful of parameters the
//! recipe allows, each step measured, every evaluation logged to <out>.jsonl. Powell is not ported
//! (`--method powell` stays in tools/mixopt.py).

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{json, Value};

type Cfg = toml::Value;

use crate::check::{self, BANDS};
use crate::nelder_mead::{self, Settings};
use crate::opt_dsp::{self as dsp, MaskRow, HOP, LEVEL_FREE_DB, N_FFT};
use crate::opt_spec::{
    absolutise, apply_values, param_table_cache, parse_loss, parse_vary, stem_config, Target, Terms,
};

pub const USAGE: &str = "Usage: sdsp-chain opt <base.toml> --vary \"track.stage.param:lo:hi\" [...] --loss \"ref:ref.wav,masking,loudness,level\"
                       [--iters 60] [--step 0.25] [--out best.toml] [--resume] [--span A:B] [--ref-span A:B]
                       [--work DIR] [--chain BIN]
  --vary  \"<track>.<stage>.<param>:<lo>:<hi>\"   stage = plugin key, `eq#2` = the second eq on that track;
          \"<track>.gain_db:lo:hi\" moves the fader; \"master.<stage>.<param>\" addresses the [[master]] chain.
          Bounds are clipped to the plugin's own range (sdsp-chain --params <plugin>).
  --loss  comma list, each term optionally `=weight`:
            ref:<wav|check.json>  band-balance distance to the reference (dB RMS, level-free)
            loudness              |LUFS gap| to the reference (needs ref:)
            masking               sum over stem pairs and 32 bus bands of the share of frames where both
                                  are > -50 dB and within 6 dB
            level                 a stem that drops >= 2 dB against the base pays the excess in dB
  --iters evaluations; 0 = measure the base and stop. --resume continues from the best point in <out>.jsonl.";

pub struct Opts {
    pub base: String,
    pub vary: Vec<String>,
    pub loss: String,
    pub iters: usize,
    pub step: f64,
    pub out: Option<String>,
    pub resume: bool,
    pub span: Option<String>,
    pub ref_span: Option<String>,
    pub work: Option<String>,
    pub chain: Option<String>,
}

pub fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        base: String::new(),
        vary: vec![],
        loss: String::new(),
        iters: 60,
        step: 0.25,
        out: None,
        resume: false,
        span: None,
        ref_span: None,
        work: None,
        chain: None,
    };
    let mut it = args.iter().peekable();
    let text = |v: Option<&String>, k: &str| -> Result<String, String> {
        v.cloned().ok_or(format!("{k} needs a value"))
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--vary" => {
                while let Some(p) = it.peek() {
                    if p.starts_with("--") {
                        break;
                    }
                    o.vary.push(it.next().cloned().unwrap_or_default());
                }
            }
            "--loss" => o.loss = text(it.next(), a)?,
            "--iters" => {
                o.iters = text(it.next(), a)?
                    .parse()
                    .map_err(|_| "--iters: an integer".to_string())?
            }
            "--step" => {
                o.step = text(it.next(), a)?
                    .parse()
                    .map_err(|_| "--step: a number".to_string())?
            }
            "--out" => o.out = Some(text(it.next(), a)?),
            "--resume" => o.resume = true,
            "--span" => o.span = Some(text(it.next(), a)?),
            "--ref-span" => o.ref_span = Some(text(it.next(), a)?),
            "--work" => o.work = Some(text(it.next(), a)?),
            "--chain" => o.chain = Some(text(it.next(), a)?),
            "--method" => {
                if text(it.next(), a)? != "nelder-mead" {
                    return Err(
                        "--method: only nelder-mead is ported; run tools/mixopt.py for powell"
                            .into(),
                    );
                }
            }
            "-h" | "--help" => return Err(USAGE.into()),
            x if o.base.is_empty() && !x.starts_with("--") => o.base = x.to_string(),
            x => return Err(format!("unexpected argument {x:?}\n{USAGE}")),
        }
    }
    if o.base.is_empty() || o.vary.is_empty() || o.loss.is_empty() {
        return Err(USAGE.into());
    }
    Ok(o)
}

type Span = Option<(f64, Option<f64>)>;

fn parse_span(s: &Option<String>) -> Result<Span, String> {
    let Some(s) = s.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let (a, b) = s.split_once(':').unwrap_or((s, ""));
    let num = |x: &str| {
        x.parse::<f64>()
            .map_err(|_| format!("span {s:?}: numbers START:END"))
    };
    Ok(Some((
        if a.is_empty() { 0.0 } else { num(a)? },
        if b.is_empty() { None } else { Some(num(b)?) },
    )))
}

/// The cut of a stereo signal a span names.
fn cut(l: &[f64], r: &[f64], sr: f64, span: Span) -> Result<(Vec<f64>, Vec<f64>), String> {
    let Some((a, b)) = span else {
        return Ok((l.to_vec(), r.to_vec()));
    };
    let i0 = (a * sr) as usize;
    let i1 = b.map_or(l.len(), |b| (b * sr) as usize);
    if i1 <= i0 || i0 >= l.len() {
        return Err(format!(
            "empty section {a}..{b:?} s (file is {:.1} s)",
            l.len() as f64 / sr
        ));
    }
    Ok((
        l[i0..i1.min(l.len())].to_vec(),
        r[i0..i1.min(r.len())].to_vec(),
    ))
}

fn find_chain(explicit: Option<&str>) -> Option<PathBuf> {
    let env = std::env::var("SDSP_CHAIN").ok();
    [explicit.map(String::from), env]
        .into_iter()
        .flatten()
        .map(PathBuf::from)
        .chain(std::env::current_exe().ok())
        .find(|p| p.is_file())
}

/// Write cfg as TOML with `out` set, render it, return the WAV path.
pub fn render(
    chain: &Path,
    cfg: &Cfg,
    toml_path: &Path,
    out_wav: &Path,
) -> Result<PathBuf, String> {
    let mut cfg = cfg.clone();
    if let Some(t) = cfg.as_table_mut() {
        t.insert("out".into(), Cfg::String(out_wav.display().to_string()));
    }
    std::fs::write(toml_path, toml::to_string(&cfg).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let r = Command::new(chain)
        .arg(toml_path)
        .output()
        .map_err(|e| format!("sdsp-chain: {e}"))?;
    if !r.status.success() {
        let msg = String::from_utf8_lossy(if r.stderr.is_empty() {
            &r.stdout
        } else {
            &r.stderr
        })
        .trim()
        .to_string();
        return Err(format!(
            "sdsp-chain failed on {}:\n{msg}",
            toml_path.display()
        ));
    }
    Ok(out_wav.to_path_buf())
}

fn read_wav(path: &Path) -> Result<check::Audio, String> {
    check::load(path, None, None)
}

/// (centred bands, LUFS) from a WAV or a `sdsp-chain check --json` profile.
fn load_reference(path: &Path, span: Span) -> Result<(Vec<f64>, Option<f64>), String> {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    {
        let d: Value = serde_json::from_str(
            &std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?,
        )
        .map_err(|e| e.to_string())?;
        let v: Vec<f64> = BANDS
            .iter()
            .map(|(_, _, n)| {
                d["bands_db"][*n]
                    .as_f64()
                    .ok_or(format!("{}: no bands_db.{n}", path.display()))
            })
            .collect::<Result<_, _>>()?;
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        return Ok((
            v.iter().map(|x| x - mean).collect(),
            d["lufs_integrated"].as_f64(),
        ));
    }
    let a = read_wav(path)?;
    let (l, r) = cut(&a.l, &a.r, a.sr, span)?;
    let mono: Vec<f64> = l.iter().zip(&r).map(|(x, y)| (x + y) * 0.5).collect();
    Ok((
        dsp::balance_bands(&mono, a.sr),
        Some(check::lufs(&l, &r, a.sr).0),
    ))
}

fn mono_of(l: &[f64], r: &[f64]) -> Vec<f64> {
    l.iter().zip(r).map(|(x, y)| (x + y) * 0.5).collect()
}

pub struct Evaluator {
    chain: PathBuf,
    targets: Vec<Target>,
    terms: Vec<(String, f64)>,
    cfg: Cfg,
    names: Vec<String>,
    work: PathBuf,
    span: Span,
    ref_bands: Option<Vec<f64>>,
    ref_lufs: Option<f64>,
    jobs: usize,
    cache: HashMap<Vec<i64>, (f64, Terms)>,
    base_levels: Option<BTreeMap<String, Vec<f64>>>,
    pub last_masking: Vec<MaskRow>,
    pub n: usize,
    pub seconds: Vec<f64>,
}

pub struct EvalSetup<'a> {
    pub chain: &'a Path,
    pub cfg: &'a Cfg,
    pub base_dir: &'a Path,
    pub targets: Vec<Target>,
    pub terms: Vec<(String, f64)>,
    pub reference: Option<&'a Path>,
    pub work: Option<&'a Path>,
    pub span: Span,
    pub ref_span: Span,
}

impl Evaluator {
    pub fn new(s: EvalSetup) -> Result<Self, String> {
        let cfg = absolutise(s.cfg, s.base_dir);
        let names: Vec<String> = cfg
            .get("track")
            .and_then(|t| t.as_array())
            .into_iter()
            .flatten()
            .map(|t| {
                t.get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
        let mut uniq = names.clone();
        uniq.sort();
        uniq.dedup();
        if uniq.len() != names.len() || names.iter().any(|n| n.is_empty()) {
            return Err("every [[track]] needs a unique `name` for stems to be addressable".into());
        }
        let work = s
            .work
            .map(Path::to_path_buf)
            .unwrap_or_else(|| s.base_dir.join(".mixopt"));
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let (ref_bands, ref_lufs) = match s.reference {
            Some(p) => {
                let (b, l) = load_reference(p, s.ref_span)?;
                (Some(b), l)
            }
            None => (None, None),
        };
        if s.terms.iter().any(|(k, _)| k == "loudness") && ref_lufs.is_none() {
            return Err(format!(
                "loudness: {} carries no LUFS",
                s.reference
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            ));
        }
        let jobs = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .min(8);
        let mut ev = Evaluator {
            chain: s.chain.to_path_buf(),
            targets: s.targets,
            terms: s.terms,
            cfg,
            names,
            work,
            span: s.span,
            ref_bands,
            ref_lufs,
            jobs,
            cache: HashMap::new(),
            base_levels: None,
            last_masking: vec![],
            n: 0,
            seconds: vec![],
        };
        if ev.has("level") {
            ev.base_levels = Some(ev.stem_levels(&ev.cfg.clone())?);
        }
        Ok(ev)
    }

    pub fn targets(&self) -> &[Target] {
        &self.targets
    }

    fn has(&self, term: &str) -> bool {
        self.terms.iter().any(|(k, _)| k == term)
    }

    fn render_stems(&self, cfg: &Cfg) -> Result<BTreeMap<String, PathBuf>, String> {
        let mut out = BTreeMap::new();
        for chunk in self.names.chunks(self.jobs.max(1)) {
            let results: Vec<Result<(String, PathBuf), String>> = std::thread::scope(|sc| {
                let handles: Vec<_> = chunk
                    .iter()
                    .map(|name| {
                        sc.spawn(move || {
                            let safe: String = name
                                .chars()
                                .map(|c| {
                                    if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                                        c
                                    } else {
                                        '_'
                                    }
                                })
                                .collect();
                            let wav = render(
                                &self.chain,
                                &stem_config(cfg, name),
                                &self.work.join(format!("stem-{safe}.toml")),
                                &self.work.join(format!("stem-{safe}.wav")),
                            )?;
                            Ok((name.clone(), wav))
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| {
                        h.join()
                            .unwrap_or_else(|_| Err("render thread panicked".into()))
                    })
                    .collect()
            });
            for r in results {
                let (n, p) = r?;
                out.insert(n, p);
            }
        }
        Ok(out)
    }

    fn stem_levels(&self, cfg: &Cfg) -> Result<BTreeMap<String, Vec<f64>>, String> {
        let mut out = BTreeMap::new();
        for (n, p) in self.render_stems(cfg)? {
            let a = read_wav(&p)?;
            out.insert(n, dsp::avg_bands(&a.l, &a.r, a.sr));
        }
        Ok(out)
    }

    /// Band and loudness terms from the mix render.
    fn mix_terms(&mut self, cfg: &Cfg, terms: &mut Terms) -> Result<(), String> {
        let a = read_wav(&render(
            &self.chain,
            cfg,
            &self.work.join("eval.toml"),
            &self.work.join("eval.wav"),
        )?)?;
        let (l, r) = cut(&a.l, &a.r, a.sr, self.span)?;
        if let Some(rb) = self.ref_bands.as_ref().filter(|_| self.has("band")) {
            let d: Vec<f64> = dsp::balance_bands(&mono_of(&l, &r), a.sr)
                .iter()
                .zip(rb)
                .map(|(x, y)| x - y)
                .collect();
            terms.vals.push((
                "band",
                (d.iter().map(|x| x * x).sum::<f64>() / d.len() as f64).sqrt(),
            ));
        }
        if self.has("loudness") {
            let lufs = check::lufs(&l, &r, a.sr).0;
            terms.vals.push((
                "loudness",
                if lufs.is_finite() {
                    (lufs - self.ref_lufs.unwrap_or(0.0)).abs()
                } else {
                    100.0
                },
            ));
        }
        Ok(())
    }

    /// Masking and level terms from the stem renders.
    fn stem_terms(&mut self, cfg: &Cfg, terms: &mut Terms) -> Result<(), String> {
        let audio: BTreeMap<String, check::Audio> = self
            .render_stems(cfg)?
            .into_iter()
            .map(|(n, p)| Ok((n, read_wav(&p)?)))
            .collect::<Result<_, String>>()?;
        if self.has("masking") {
            let bands: BTreeMap<String, Vec<Vec<f64>>> = audio
                .iter()
                .map(|(n, a)| {
                    (
                        n.clone(),
                        dsp::bus_bands(&mono_of(&a.l, &a.r), a.sr, N_FFT, HOP),
                    )
                })
                .collect();
            let shares = dsp::masking_shares(&bands);
            terms.vals.push((
                "masking",
                shares.iter().map(|(_, s)| s.iter().sum::<f64>()).sum(),
            ));
            self.last_masking = dsp::masking_report(&shares);
        }
        if let Some(base) = self.base_levels.as_ref().filter(|_| self.has("level")) {
            let drop: BTreeMap<&String, f64> = audio
                .iter()
                .map(|(n, a)| {
                    (
                        n,
                        dsp::broadband_drop(&base[n], &dsp::avg_bands(&a.l, &a.r, a.sr)),
                    )
                })
                .collect();
            terms.vals.push((
                "level",
                drop.values().map(|d| (d - LEVEL_FREE_DB).max(0.0)).sum(),
            ));
            terms.levels = Some(
                drop.iter()
                    .map(|(n, d)| ((*n).clone(), (-d * 100.0).round() / 100.0))
                    .collect(),
            );
        }
        Ok(())
    }

    /// Render a candidate and return (loss, terms). Deterministic renderer + pure measurement = cached.
    pub fn measure(&mut self, values: &[f64]) -> Result<(f64, Terms), String> {
        let key: Vec<i64> = values.iter().map(|v| (v * 1e6).round() as i64).collect();
        if let Some(hit) = self.cache.get(&key) {
            return Ok(hit.clone());
        }
        let t0 = Instant::now();
        let cfg = apply_values(&self.cfg, &self.targets, values)?;
        let mut terms = Terms::default();
        if self.has("band") || self.has("loudness") {
            self.mix_terms(&cfg, &mut terms)?;
        }
        if self.has("masking") || self.has("level") {
            self.stem_terms(&cfg, &mut terms)?;
        }
        let loss: f64 = terms
            .vals
            .iter()
            .map(|(k, v)| {
                self.terms
                    .iter()
                    .find(|(t, _)| t == k)
                    .map_or(0.0, |(_, w)| w * v)
            })
            .sum();
        self.n += 1;
        self.seconds.push(t0.elapsed().as_secs_f64());
        self.cache.insert(key, (loss, terms.clone()));
        Ok((loss, terms))
    }
}

// ---------------------------------------------------------------------------
// Optimiser loop
// ---------------------------------------------------------------------------

enum Stop {
    Budget,
    Fail(String),
}

pub struct Outcome {
    pub best: Option<Vec<f64>>,
    pub best_loss: f64,
    pub history: Vec<Value>,
}

fn r4(x: f64) -> Value {
    json!((x * 1e4).round() / 1e4)
}

fn record(
    ev: &Evaluator,
    targets: &[Target],
    v: &[f64],
    loss: f64,
    terms: &Terms,
    k: usize,
    fresh_s: Option<f64>,
) -> Value {
    let mut tm = serde_json::Map::new();
    for (a, b) in &terms.vals {
        tm.insert(a.to_string(), r4(*b));
    }
    if let Some(l) = &terms.levels {
        tm.insert("_levels".into(), json!(l));
    }
    let values: serde_json::Map<String, Value> = targets
        .iter()
        .zip(v)
        .map(|(t, x)| (t.key.clone(), r4(*x)))
        .collect();
    let diff: serde_json::Map<String, Value> = targets
        .iter()
        .zip(v)
        .filter(|(t, x)| (**x - t.start).abs() > 1e-9)
        .map(|(t, x)| (t.key.clone(), json!([r4(t.start), r4(*x)])))
        .collect();
    let _ = ev;
    json!({"n": k, "loss": (loss * 1e6).round() / 1e6, "terms": tm, "values": values, "diff": diff, "s": fresh_s.map(|s| (s * 1e3).round() / 1e3)})
}

/// Bounded Nelder-Mead on x in [0,1]^n (each target normalised to its bounds). Appends every evaluation to `log`.
pub fn optimize(
    ev: &mut Evaluator,
    targets: &[Target],
    iters: usize,
    step: f64,
    log: Option<&Path>,
    start: Option<&[f64]>,
    quiet: bool,
) -> Result<Outcome, String> {
    let lo: Vec<f64> = targets.iter().map(|t| t.lo).collect();
    let hi: Vec<f64> = targets.iter().map(|t| t.hi).collect();
    let x0: Vec<f64> = start
        .map(<[f64]>::to_vec)
        .unwrap_or_else(|| targets.iter().map(|t| t.clip(t.start)).collect());
    let u0: Vec<f64> = x0
        .iter()
        .enumerate()
        .map(|(i, x)| (x - lo[i]) / (hi[i] - lo[i]))
        .collect();
    let mut logf = match log {
        Some(p) => Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .map_err(|e| e.to_string())?,
        ),
        None => None,
    };
    let mut out = Outcome {
        best: None,
        best_loss: f64::INFINITY,
        history: vec![],
    };
    let mut best_n = 0;
    let mut f = |u: &[f64]| -> Result<f64, Stop> {
        if iters > 0 && out.history.len() >= iters {
            return Err(Stop::Budget);
        }
        let v: Vec<f64> = u
            .iter()
            .enumerate()
            .map(|(i, x)| lo[i] + x.clamp(0.0, 1.0) * (hi[i] - lo[i]))
            .collect();
        let before = ev.n;
        let (loss, terms) = ev.measure(&v).map_err(Stop::Fail)?;
        let k = out.history.len() + 1;
        let rec = record(
            ev,
            targets,
            &v,
            loss,
            &terms,
            k,
            (ev.n != before).then(|| ev.seconds[ev.seconds.len() - 1]),
        );
        if let Some(f) = logf.as_mut() {
            let _ = writeln!(f, "{rec}");
        }
        out.history.push(rec);
        if loss < out.best_loss {
            out.best_loss = loss;
            out.best = Some(v.clone());
            best_n = k;
        }
        if !quiet {
            let tt: Vec<String> = terms
                .vals
                .iter()
                .map(|(a, b)| format!("{a}={b:.3}"))
                .collect();
            let vv: Vec<String> = targets
                .iter()
                .zip(&v)
                .map(|(t, x)| format!("{}={}", t.key, crate::opt_spec::g3(*x)))
                .collect();
            println!(
                "  {k:>3}  loss {loss:8.4}  [{}]  {}{}",
                tt.join("  "),
                vv.join("  "),
                if k == best_n { "   *" } else { "" }
            );
        }
        Ok(loss)
    };
    let run: Result<(), Stop> = if iters == 0 {
        f(&u0).map(|_| ())
    } else {
        let mut simplex = vec![u0.clone()];
        for i in 0..u0.len() {
            let mut e = u0.clone();
            e[i] = if e[i] + step <= 1.0 {
                e[i] + step
            } else {
                e[i] - step
            };
            simplex.push(e);
        }
        nelder_mead::minimize(simplex, &Settings::default(), &mut f).map(|_| ())
    };
    match run {
        Ok(()) | Err(Stop::Budget) => Ok(out),
        Err(Stop::Fail(e)) => Err(e),
    }
}

fn read_log(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter(|r| r.get("loss").is_some())
                .collect()
        })
        .unwrap_or_default()
}

pub fn run(args: &[String]) -> Result<(), String> {
    let o = parse_args(args)?;
    let chain = find_chain(o.chain.as_deref())
        .ok_or("sdsp-chain not found: pass --chain or set SDSP_CHAIN")?;
    let base_path = Path::new(&o.base)
        .canonicalize()
        .map_err(|e| format!("{}: {e}", o.base))?;
    let base_dir = base_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut cfg: Cfg =
        toml::from_str(&std::fs::read_to_string(&base_path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{}: {e}", o.base))?;
    if cfg
        .get("track")
        .and_then(|t| t.as_array())
        .is_none_or(|t| t.is_empty())
    {
        return Err(format!(
            "{}: needs [[track]] entries (the single-chain form has no stems)",
            o.base
        ));
    }
    let (terms, mut reference) = parse_loss(&o.loss)?;
    if let Some(r) = reference.as_mut() {
        if !Path::new(r.as_str()).is_absolute() && !Path::new(r.as_str()).exists() {
            *r = base_dir.join(&r).display().to_string();
        }
    }
    let mut tables = param_table_cache();
    let targets: Vec<Target> = o
        .vary
        .iter()
        .map(|s| parse_vary(s, &mut cfg, &chain, &mut tables))
        .collect::<Result<_, _>>()?;
    let out = o
        .out
        .as_ref()
        .map(PathBuf::from)
        .map(|p| resolve_out(&p))
        .unwrap_or_else(|| {
            base_path.with_file_name(format!(
                "{}.best.toml",
                base_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("mix")
            ))
        });
    let log = out.with_extension("jsonl");
    print_header(&base_path, &chain, &terms, reference.as_deref(), &targets);
    let work = o.work.as_ref().map(PathBuf::from);
    let mut ev = Evaluator::new(EvalSetup {
        chain: &chain,
        cfg: &cfg,
        base_dir: &base_dir,
        targets: targets.clone(),
        terms: terms.clone(),
        reference: reference.as_deref().map(Path::new),
        work: work.as_deref(),
        span: parse_span(&o.span)?,
        ref_span: parse_span(&o.ref_span)?,
    })?;
    let (start, done) = resume_point(&o, &log, &targets, &base_path, &chain);
    let iters = o.iters.saturating_sub(done);
    let t0 = Instant::now();
    let res = optimize(
        &mut ev,
        &targets,
        iters,
        o.step,
        Some(&log),
        start.as_deref(),
        false,
    )?;
    print_summary(&ev, &targets, &res, t0.elapsed().as_secs_f64());
    if let (Some(best), true) = (res.best.as_ref(), iters > 0) {
        write_best(&ev, &cfg, &targets, best, &out, &base_dir, &chain)?;
    }
    Ok(())
}

/// Absolute path with symlinks in the folder resolved (the file may not exist yet), so it compares equal to the base's folder.
fn resolve_out(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    match (
        abs.parent().and_then(|d| d.canonicalize().ok()),
        abs.file_name(),
    ) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => abs,
    }
}

fn print_header(
    base: &Path,
    chain: &Path,
    terms: &[(String, f64)],
    reference: Option<&str>,
    targets: &[Target],
) {
    println!(
        "sdsp-chain opt  {}  chain={}",
        base.file_name().and_then(|n| n.to_str()).unwrap_or(""),
        chain.display()
    );
    let t: Vec<String> = terms
        .iter()
        .map(|(k, w)| format!("{k}×{}", crate::opt_spec::g3(*w)))
        .collect();
    println!(
        "  loss: {}{}",
        t.join(", "),
        reference.map(|r| format!("  ref={r}")).unwrap_or_default()
    );
    for t in targets {
        println!(
            "  vary: {:<28} {:>9} … {:<9} start {}",
            t.key,
            crate::opt_spec::g3(t.lo),
            crate::opt_spec::g3(t.hi),
            crate::opt_spec::g4(t.start)
        );
    }
}

/// With --resume: the best logged point and how many evaluations the log holds; otherwise a fresh log with its header line.
fn resume_point(
    o: &Opts,
    log: &Path,
    targets: &[Target],
    base: &Path,
    chain: &Path,
) -> (Option<Vec<f64>>, usize) {
    let (mut start, mut done) = (None, 0);
    if o.resume {
        let prev = read_log(log);
        let best = prev.iter().min_by(|a, b| {
            a["loss"]
                .as_f64()
                .partial_cmp(&b["loss"].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if let Some(b) = best {
            start = targets
                .iter()
                .map(|t| b["values"][&t.key].as_f64())
                .collect();
            done = prev.len();
            println!(
                "  resume: {done} evaluations logged, best loss {:.4} at #{}",
                b["loss"].as_f64().unwrap_or(0.0),
                b["n"]
            );
        }
    } else if log.exists() {
        let _ = std::fs::remove_file(log);
    }
    if done == 0 {
        let head = json!({"base": base.display().to_string(), "vary": o.vary, "loss_spec": o.loss, "chain": chain.display().to_string()});
        let _ = std::fs::write(log, format!("{head}\n"));
    }
    (start, done)
}

fn print_summary(ev: &Evaluator, targets: &[Target], res: &Outcome, wall: f64) {
    if ev.seconds.is_empty() {
        println!();
    } else {
        println!(
            "\n{} evaluations in {wall:.1} s ({:.2} s each, {} tracks)",
            res.history.len(),
            ev.seconds.iter().sum::<f64>() / ev.seconds.len() as f64,
            ev.names.len()
        );
    }
    let base_loss = res.history.first().and_then(|r| r["loss"].as_f64());
    if let Some(b) = base_loss.filter(|b| *b > 0.0) {
        println!(
            "  loss {b:.4} → {:.4} ({:+.0}%)",
            res.best_loss,
            100.0 * (1.0 - res.best_loss / b)
        );
    }
    let (mut traj, mut cur) = (vec![], f64::INFINITY);
    for r in &res.history {
        let l = r["loss"].as_f64().unwrap_or(f64::INFINITY);
        if l < cur {
            cur = l;
            traj.push(format!("#{} {}", r["n"], crate::opt_spec::g4(cur)));
        }
    }
    println!("  best-so-far: {}", traj.join(" → "));
    for (i, t) in targets.iter().enumerate() {
        let v = res.best.as_ref().map_or(t.start, |b| b[i]);
        println!(
            "  {:<28} {:>9} → {:<9}",
            t.key,
            crate::opt_spec::g4(t.start),
            crate::opt_spec::g4(v)
        );
    }
    for row in ev
        .last_masking
        .iter()
        .take(5)
        .filter(|r| r.worst_share > 0.05)
    {
        println!(
            "  clash {} × {}: {}-{} Hz {:.0}% of the time (Σ bands {:.2})",
            row.a,
            row.b,
            row.hz.0,
            row.hz.1,
            100.0 * row.worst_share,
            row.total
        );
    }
}

fn write_best(
    ev: &Evaluator,
    cfg: &Cfg,
    targets: &[Target],
    best: &[f64],
    out: &Path,
    base_dir: &Path,
    chain: &Path,
) -> Result<(), String> {
    // Relative inputs survive only next to the base; elsewhere pin them.
    let keep = if out.parent() == Some(base_dir) {
        cfg
    } else {
        &ev.cfg
    };
    std::fs::write(
        out,
        toml::to_string(&apply_values(keep, targets, best)?).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let wav = out.with_extension("wav");
    render(
        chain,
        &apply_values(&ev.cfg, targets, best)?,
        &ev.work.join("best.toml"),
        &wav,
    )?;
    println!(
        "  wrote {} and {}; log {}. Listen before keeping it.",
        out.display(),
        wav.file_name().and_then(|n| n.to_str()).unwrap_or(""),
        out.with_extension("jsonl")
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
    );
    Ok(())
}
