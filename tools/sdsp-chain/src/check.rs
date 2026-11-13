//! `sdsp-chain check`: measure a finished mix, optionally against a reference.
//!
//! Rust port of `tools/mixcheck.py` (same numbers: band balance, crest, LUFS, correlation,
//! per-band mono loss, section arc, tonal-balance corridor). Pure Rust, no Python at runtime.
//! Golden tests below were checked against the Python tool on the same signals, tolerance
//! 0.1 dB / 0.1 LU (the loudness crate re-derives K-weighting per sample rate, < 0.05 LU).

use std::path::Path;

use realfft::RealFftPlanner;
use serde_json::{json, Map, Value};
use superduper_synth_core::wav::parse_wav_file;

pub const BANDS: [(f64, f64, &str); 8] = [
    (20.0, 60.0, "sub"),
    (60.0, 120.0, "kick"),
    (120.0, 250.0, "low"),
    (250.0, 800.0, "body"),
    (800.0, 2500.0, "mid"),
    (2500.0, 6000.0, "presence"),
    (6000.0, 16000.0, "air"),
    (16000.0, 20000.0, "top"),
];
const N_FFT: usize = 4096;
const HOP_FRAMES: usize = 8;

pub struct Audio {
    pub l: Vec<f64>,
    pub r: Vec<f64>,
    pub sr: f64,
}

pub fn load(path: &Path, start: Option<f64>, end: Option<f64>) -> Result<Audio, String> {
    let w = parse_wav_file(path).map_err(|e| format!("{}: {e:?}", path.display()))?;
    let ch = w.channels as usize;
    let n = w.samples.len() / ch.max(1);
    let (mut l, mut r) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for i in 0..n {
        l.push(w.samples[i * ch] as f64);
        r.push(w.samples[i * ch + if ch > 1 { 1 } else { 0 }] as f64);
    }
    let sr = w.sample_rate as f64;
    let i0 = (start.unwrap_or(0.0) * sr) as usize;
    let i1 = end.map(|e| (e * sr) as usize).unwrap_or(n).min(n);
    if i1 <= i0 || i0 >= n {
        return Err(format!(
            "{}: empty section {:?}..{:?} s (file is {:.1} s)",
            path.display(),
            start,
            end,
            n as f64 / sr
        ));
    }
    Ok(Audio { l: l[i0..i1].to_vec(), r: r[i0..i1].to_vec(), sr })
}

fn db(x: f64) -> f64 {
    20.0 * (x.abs() + 1e-12).log10()
}

fn rms(s: &[f64]) -> f64 {
    (s.iter().map(|v| v * v).sum::<f64>() / s.len().max(1) as f64).sqrt()
}

fn hann(n: usize) -> Vec<f64> {
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos()).collect()
}

/// Band energy relative to the loudest band, dB. Frames every `N_FFT * HOP_FRAMES` samples, as the Python tool.
pub fn band_energy(mono: &[f64], sr: f64) -> Vec<f64> {
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(N_FFT);
    let win = hann(N_FFT);
    let mut acc = [0.0f64; 8];
    let step = N_FFT * HOP_FRAMES;
    let mut s = 0;
    while s < mono.len().saturating_sub(N_FFT).max(1) {
        let mut buf = vec![0.0; N_FFT];
        for i in 0..N_FFT {
            buf[i] = mono.get(s + i).copied().unwrap_or(0.0) * win[i];
        }
        let mut spec = fft.make_output_vec();
        let _ = fft.process(&mut buf, &mut spec);
        for (b, (lo, hi, _)) in BANDS.iter().enumerate() {
            let i0 = (lo * N_FFT as f64 / sr) as usize;
            let i1 = ((hi * N_FFT as f64 / sr) as usize + 1).min(N_FFT / 2);
            acc[b] += (i0..i1).map(|i| spec[i].norm_sqr()).sum::<f64>();
        }
        s += step;
    }
    let top = acc.iter().cloned().fold(0.0, f64::max);
    let top = if top == 0.0 { 1.0 } else { top };
    acc.iter().map(|v| 10.0 * (v / top + 1e-12).log10()).collect()
}

/// Direct-form-I biquad, f64, coefficients (b0,b1,b2,a1,a2) with a0 = 1.
fn biquad(x: &[f64], c: [f64; 5]) -> Vec<f64> {
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    x.iter()
        .map(|&v| {
            let y = c[0] * v + c[1] * x1 + c[2] * x2 - c[3] * y1 - c[4] * y2;
            x2 = x1;
            x1 = v;
            y2 = y1;
            y1 = y;
            y
        })
        .collect()
}

/// K-weighting for any sample rate, same derivation as mixcheck.py: high shelf (+4 dB above ~1.7 kHz), then the RLB high-pass at ~38 Hz.
fn k_weight(x: &[f64], sr: f64) -> Vec<f64> {
    use std::f64::consts::PI;
    let k = (PI * 1681.974450955533 / sr).tan();
    let (vh, q) = (10f64.powf(3.999843853973347 / 20.0), 0.7071752369554196);
    let vb = vh.powf(0.4996667741545416);
    let a0 = 1.0 + k / q + k * k;
    let shelf = [(vh + vb * k / q + k * k) / a0, 2.0 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0];
    let y = biquad(x, shelf);
    let k = (PI * 38.13547087602444 / sr).tan();
    let q = 0.5003270373238773;
    let a0 = 1.0 + k / q + k * k;
    biquad(&y, [1.0, -2.0, 1.0, 2.0 * (k * k - 1.0) / a0, (1.0 - k / q + k * k) / a0])
}

/// (integrated, loudest 3 s) LUFS per BS.1770-4: K-weighting, 400 ms blocks at 100 ms hop, -70 / -10 LU gates.
pub fn lufs(l: &[f64], r: &[f64], sr: f64) -> (f64, f64) {
    let (kl, kr) = (k_weight(l, sr), k_weight(r, sr));
    let sq: Vec<f64> = kl.iter().zip(&kr).map(|(x, y)| x * x + y * y).collect();
    let mut cum = Vec::with_capacity(sq.len() + 1);
    cum.push(0.0);
    for v in &sq {
        cum.push(cum.last().unwrap() + v);
    }
    let blocks = |seconds: f64, step: f64| -> Vec<f64> {
        let (w, h) = ((seconds * sr) as usize, (step * sr) as usize);
        if sq.len() < w {
            return vec![cum[sq.len()] / sq.len().max(1) as f64];
        }
        (0..=(sq.len() - w)).step_by(h.max(1)).map(|s| (cum[s + w] - cum[s]) / w as f64).collect()
    };
    let loud = |ms: f64| -0.691 + 10.0 * (ms + 1e-20).log10();
    let gate: Vec<f64> = blocks(0.4, 0.1).into_iter().filter(|b| loud(*b) > -70.0).collect();
    if gate.is_empty() {
        return (f64::NEG_INFINITY, f64::NEG_INFINITY);
    }
    let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
    let rel = loud(mean(&gate)) - 10.0;
    let kept: Vec<f64> = gate.iter().cloned().filter(|b| loud(*b) > rel).collect();
    let short = blocks(3.0, 1.0).into_iter().map(loud).fold(f64::NEG_INFINITY, f64::max);
    (loud(mean(&kept)), short)
}

/// Per band dB lost folding to mono, and side/mid under 120 Hz (None when there is no low end).
pub fn mono_loss(l: &[f64], r: &[f64], sr: f64) -> (Vec<(&'static str, f64)>, Option<f64>) {
    let n = (l.len().min(r.len()) / N_FFT) * N_FFT;
    if n == 0 {
        return (vec![], None);
    }
    let mut planner = RealFftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(N_FFT);
    let win = hann(N_FFT);
    let bins = N_FFT / 2 + 1;
    let (mut el, mut er, mut em, mut es) = (vec![0.0; bins], vec![0.0; bins], vec![0.0; bins], vec![0.0; bins]);
    for f in 0..n / N_FFT {
        let spec = |x: &[f64]| {
            let mut buf: Vec<f64> = (0..N_FFT).map(|i| x[f * N_FFT + i] * win[i]).collect();
            let mut out = fft.make_output_vec();
            let _ = fft.process(&mut buf, &mut out);
            out
        };
        let (fl, fr) = (spec(l), spec(r));
        for k in 0..bins {
            el[k] += fl[k].norm_sqr();
            er[k] += fr[k].norm_sqr();
            em[k] += ((fl[k] + fr[k]) * 0.5).norm_sqr();
            es[k] += ((fl[k] - fr[k]) * 0.5).norm_sqr();
        }
    }
    let hz = |k: usize| k as f64 * sr / N_FFT as f64;
    let sum = |v: &[f64], pred: &dyn Fn(f64) -> bool| (0..bins).filter(|k| pred(hz(*k))).map(|k| v[k]).sum::<f64>();
    let mut out = Vec::new();
    for (lo, hi, name) in BANDS.iter() {
        let p = |h: f64| h >= *lo && h < *hi;
        let stereo = (sum(&el, &p) + sum(&er, &p)) / 2.0;
        if stereo > 1e-12 {
            out.push((*name, 10.0 * (sum(&em, &p) / stereo + 1e-20).log10()));
        }
    }
    let low = |h: f64| h < 120.0;
    let (m, s) = (sum(&em, &low), sum(&es, &low));
    if m + s < 1e-12 {
        return (out, None);
    }
    (out, Some(10.0 * ((s + 1e-20) / (m + 1e-20)).log10()))
}

fn correlation(l: &[f64], r: &[f64]) -> f64 {
    let num: f64 = l.iter().zip(r).map(|(a, b)| a * b).sum();
    let den = (l.iter().map(|v| v * v).sum::<f64>() * r.iter().map(|v| v * v).sum::<f64>()).sqrt();
    num / if den == 0.0 { 1.0 } else { den }
}

fn mono_of(l: &[f64], r: &[f64]) -> Vec<f64> {
    l.iter().zip(r).map(|(a, b)| (a + b) * 0.5).collect()
}

fn peak_of(l: &[f64], r: &[f64]) -> f64 {
    l.iter().chain(r).fold(0.0, |m, v| m.max(v.abs()))
}

fn centred(bands: &[f64]) -> Vec<f64> {
    let m = bands.iter().sum::<f64>() / bands.len() as f64;
    bands.iter().map(|b| b - m).collect()
}

/// Per band (mean, spread) of mean-centred reference profiles; spread never under 1 dB.
pub fn target_corridor(profiles: &[Vec<f64>]) -> Result<Vec<(f64, f64)>, String> {
    if profiles.is_empty() {
        return Err("no reference profiles".into());
    }
    let cs: Vec<Vec<f64>> = profiles.iter().map(|p| centred(p)).collect();
    Ok((0..BANDS.len())
        .map(|b| {
            let xs: Vec<f64> = cs.iter().map(|c| c[b]).collect();
            let mean = xs.iter().sum::<f64>() / xs.len() as f64;
            let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / xs.len() as f64;
            (mean, var.sqrt().max(1.0))
        })
        .collect())
}

/// Per band distance outside the corridor (0 inside, positive = too much of that band).
pub fn target_verdict(bands: &[f64], corridor: &[(f64, f64)]) -> Vec<f64> {
    let c = centred(bands);
    (0..BANDS.len())
        .map(|b| {
            let d = c[b] - corridor[b].0;
            let s = corridor[b].1;
            if d.abs() <= s {
                0.0
            } else if d > 0.0 {
                d - s
            } else {
                d + s
            }
        })
        .collect()
}

pub fn load_profiles(paths: &[String]) -> Result<Vec<Vec<f64>>, String> {
    let mut files = Vec::new();
    for p in paths {
        let p = Path::new(p);
        if p.is_dir() {
            let mut v: Vec<_> = std::fs::read_dir(p).map_err(|e| e.to_string())?.filter_map(|e| e.ok()).map(|e| e.path()).filter(|x| x.extension().is_some_and(|e| e == "json")).collect();
            v.sort();
            files.extend(v);
        } else {
            files.push(p.to_path_buf());
        }
    }
    files
        .iter()
        .map(|f| {
            let v: Value = serde_json::from_str(&std::fs::read_to_string(f).map_err(|e| e.to_string())?).map_err(|e| format!("{}: {e}", f.display()))?;
            let b = v.get("bands_db").and_then(|b| b.as_object()).ok_or_else(|| format!("{}: not a mixcheck profile (no bands_db)", f.display()))?;
            Ok(BANDS.iter().map(|(_, _, n)| b.get(*n).and_then(|x| x.as_f64()).unwrap_or(0.0)).collect())
        })
        .collect()
}

/// Every number the report prints, as JSON (`--json`).
pub fn profile(path: &Path, start: Option<f64>, end: Option<f64>) -> Result<Value, String> {
    let a = load(path, start, end)?;
    let n = a.l.len().min(a.r.len());
    let (l, r) = (&a.l[..n], &a.r[..n]);
    let mono = mono_of(l, r);
    let peak = peak_of(l, r);
    let bands = band_energy(&mono, a.sr);
    let (li, ls) = lufs(l, r, a.sr);
    let (loss, width) = mono_loss(l, r, a.sr);
    let finite = |v: f64| if v.is_finite() { json!(v) } else { Value::Null };
    let mut bm = Map::new();
    for (i, (_, _, name)) in BANDS.iter().enumerate() {
        bm.insert(name.to_string(), json!(bands[i]));
    }
    let mut lm = Map::new();
    for (k, v) in loss {
        lm.insert(k.to_string(), json!(v));
    }
    Ok(json!({
        "file": path.display().to_string(), "start": start, "end": end, "seconds": n as f64 / a.sr, "sr": a.sr,
        "peak_db": db(peak), "rms_db": db(rms(&mono)), "crest_db": db(peak) - db(rms(&mono)),
        "correlation": correlation(l, r), "bands_db": bm,
        "lufs_integrated": finite(li), "lufs_short_max": finite(ls),
        "mono_loss_db": lm, "side_mid_low_db": width.map(finite).unwrap_or(Value::Null),
    }))
}

pub struct Opts {
    pub mix: String,
    pub reference: Option<String>,
    pub span: (Option<f64>, Option<f64>),
    pub ref_span: (Option<f64>, Option<f64>),
    pub bpm: Option<f64>,
    pub bars: Option<Vec<usize>>,
    pub json: bool,
    pub target: Vec<String>,
}

pub fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts { mix: String::new(), reference: None, span: (None, None), ref_span: (None, None), bpm: None, bars: None, json: false, target: vec![] };
    let mut it = args.iter().peekable();
    let num = |v: Option<&String>, k: &str| -> Result<f64, String> { v.ok_or(format!("{k} needs a value"))?.parse::<f64>().map_err(|_| format!("{k}: not a number")) };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--ref" => o.reference = Some(it.next().ok_or("--ref needs a file")?.clone()),
            "--start" => o.span.0 = Some(num(it.next(), a)?),
            "--end" => o.span.1 = Some(num(it.next(), a)?),
            "--ref-start" => o.ref_span.0 = Some(num(it.next(), a)?),
            "--ref-end" => o.ref_span.1 = Some(num(it.next(), a)?),
            "--bpm" => o.bpm = Some(num(it.next(), a)?),
            "--bars" => o.bars = Some(it.next().ok_or("--bars needs a list")?.split(',').map(|x| x.trim().parse::<usize>().map_err(|_| "--bars: integers".to_string())).collect::<Result<_, _>>()?),
            "--json" => o.json = true,
            "--target" => {
                while let Some(p) = it.peek() {
                    if p.starts_with("--") {
                        break;
                    }
                    o.target.push(it.next().unwrap().clone());
                }
            }
            "-h" | "--help" => return Err(USAGE.into()),
            x if o.mix.is_empty() && !x.starts_with("--") => o.mix = x.to_string(),
            x => return Err(format!("unexpected argument {x:?}\n{USAGE}")),
        }
    }
    if o.mix.is_empty() {
        return Err(USAGE.into());
    }
    Ok(o)
}

pub const USAGE: &str = "Usage: sdsp-chain check <mix.wav> [--ref ref.wav] [--start S --end E] [--ref-start S --ref-end E]\n                          [--bpm B --bars 8,16,32] [--target profile.json|dir ...] [--json]";

pub fn run(args: &[String]) -> Result<(), String> {
    let o = parse_args(args)?;
    let mix = Path::new(&o.mix);
    if o.json {
        let mut p = profile(mix, o.span.0, o.span.1)?;
        if !o.target.is_empty() {
            let refs = load_profiles(&o.target)?;
            let cor = target_corridor(&refs)?;
            let bands: Vec<f64> = BANDS.iter().map(|(_, _, n)| p["bands_db"][*n].as_f64().unwrap_or(0.0)).collect();
            let v = target_verdict(&bands, &cor);
            let (mut c, mut out) = (Map::new(), Map::new());
            for (i, (_, _, n)) in BANDS.iter().enumerate() {
                c.insert(n.to_string(), json!({"mean": cor[i].0, "spread": cor[i].1}));
                out.insert(n.to_string(), json!(v[i]));
            }
            p["target"] = json!({"profiles": refs.len(), "corridor_db": c, "outside_db": out});
        }
        println!("{}", serde_json::to_string_pretty(&p).map_err(|e| e.to_string())?);
        return Ok(());
    }
    report(&o)
}

fn report(o: &Opts) -> Result<(), String> {
    let a = load(Path::new(&o.mix), o.span.0, o.span.1)?;
    let n = a.l.len().min(a.r.len());
    let (l, r) = (&a.l[..n], &a.r[..n]);
    let mono = mono_of(l, r);
    let peak = peak_of(l, r);
    let crest = db(peak) - db(rms(&mono));
    let name = Path::new(&o.mix).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    println!("\n── {name}   {:.1} s @ {:.0} Hz", n as f64 / a.sr, a.sr);
    let note = if crest < 8.0 { "   ← squashed; the chain is flattening the performance" } else if crest > 14.0 { "   ← very open; fine, unless it feels quiet next to the reference" } else { "" };
    println!("   peak {:+6.2} dBFS    rms {:+6.2} dB    crest {:5.1} dB{note}", db(peak), db(rms(&mono)), crest);
    let (integrated, short_max) = lufs(l, r, a.sr);
    println!("   loudness {integrated:6.1} LUFS integrated    {short_max:6.1} LUFS loudest 3 s");
    let corr = correlation(l, r);
    println!("   correlation {corr:+.2}{}", if corr < 0.2 { "   ← sides cancel in mono" } else { "" });
    print_mono(l, r, a.sr);
    let mine = band_energy(&mono, a.sr);
    let theirs = match &o.reference {
        Some(rf) => {
            let b = load(Path::new(rf), o.ref_span.0, o.ref_span.1)?;
            let rn = b.l.len().min(b.r.len());
            let (ri, _) = lufs(&b.l[..rn], &b.r[..rn], b.sr);
            let d = integrated - ri;
            println!("   reference {ri:6.1} LUFS — this mix is {d:+.1} LU{}", if d.abs() > 1.0 { "   ← gain-match before comparing by ear" } else { "" });
            Some(band_energy(&mono_of(&b.l[..rn], &b.r[..rn]), b.sr))
        }
        None => None,
    };
    print_bands(&mine, theirs.as_deref());
    if !o.target.is_empty() {
        let refs = load_profiles(&o.target)?;
        print_target(&mine, &target_corridor(&refs)?, refs.len());
    }
    if let (Some(bpm), Some(bars)) = (o.bpm, &o.bars) {
        print_arc(&mono, a.sr, bpm, bars);
    }
    Ok(())
}

fn print_mono(l: &[f64], r: &[f64], sr: f64) {
    let (loss, width) = mono_loss(l, r, sr);
    let worst = loss.iter().map(|x| x.1).fold(f64::INFINITY, f64::min);
    let cells: Vec<String> = loss.iter().map(|(k, v)| format!("{k} {v:+.1}")).collect();
    println!("   mono loss  {} dB{}", cells.join("  "), if worst < -4.5 { "   ← a band cancels in mono" } else { "" });
    match width {
        None => println!("   side/mid under 120 Hz: no low end"),
        Some(w) => println!("   side/mid under 120 Hz {w:+.1} dB{}", if w > -12.0 { "   ← wide bass" } else { "" }),
    }
}

fn print_bands(mine: &[f64], theirs: Option<&[f64]>) {
    println!("\n   band            this mix{}", if theirs.is_some() { "      reference    delta" } else { "" });
    for (i, (lo, hi, name)) in BANDS.iter().enumerate() {
        let mut line = format!("   {name:9} {:5}-{:<5} {:6.1} dB", *lo as i64, *hi as i64, mine[i]);
        if let Some(t) = theirs {
            let d = mine[i] - t[i];
            line += &format!("   {:6.1} dB   {d:+5.1}{}", t[i], if d.abs() >= 4.0 { "  ←" } else { "" });
        }
        println!("{line}");
    }
    if theirs.is_some() {
        println!("   (delta ≥ 4 dB is audible as 'darker' / 'harsher' than the reference)");
    }
}

fn print_target(mine: &[f64], cor: &[(f64, f64)], n_refs: usize) {
    let v = target_verdict(mine, cor);
    let c = centred(mine);
    println!("\n   target corridor from {n_refs} reference profile(s), level-independent (each profile mean-centred)");
    println!("   band            this mix    target       verdict");
    for (i, (_, _, name)) in BANDS.iter().enumerate() {
        let tag = if v[i] == 0.0 { "inside".to_string() } else if v[i] > 0.0 { format!("above by {:.1} dB  ←", v[i]) } else { format!("below by {:.1} dB  ←", -v[i]) };
        println!("   {name:9} {:+6.1} dB   {:+6.1} ±{:3.1}   {tag}", c[i], cor[i].0, cor[i].1);
    }
    println!("   (move EQ / Multiband only where a band is outside; inside the corridor is taste)");
}

fn print_arc(mono: &[f64], sr: f64, bpm: f64, bars: &[usize]) {
    let bar_s = 60.0 / bpm * 4.0;
    println!("\n   section arc @ {bpm:.0} BPM");
    let mut edges = vec![0usize];
    edges.extend(bars);
    let mut prev: Option<(String, f64)> = None;
    for w in edges.windows(2) {
        let (i0, i1) = ((w[0] as f64 * bar_s * sr) as usize, ((w[1] as f64 * bar_s * sr) as usize).min(mono.len()));
        let d = db(rms(&mono[i0.min(i1)..i1]));
        let name = format!("bars {}-{}", w[0], w[1]);
        let arrow = prev.as_ref().map(|(pn, pd)| format!("   {:+5.1} dB vs {pn}", d - pd)).unwrap_or_default();
        println!("   {name:12} bars {:3}-{:<3} {d:6.1} dB {}{arrow}", w[0], w[1], "█".repeat((62.0 + d).max(0.0) as usize));
        prev = Some((name, d));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(sr: f64, hz: f64, amp: f64, secs: f64, phase: f64) -> Vec<f64> {
        (0..(secs * sr) as usize).map(|i| amp * (2.0 * std::f64::consts::PI * hz * i as f64 / sr + phase).sin()).collect()
    }

    #[test]
    fn lufs_reads_ebu_tech_3341_case_1() {
        let x = sine(48000.0, 1000.0, 10f64.powf(-23.0 / 20.0), 5.0, 0.0);
        let (i, s) = lufs(&x, &x, 48000.0);
        assert!((i + 23.0).abs() < 0.1 && (s + 23.0).abs() < 0.1, "{i} {s}");
    }

    #[test]
    fn lufs_gates_out_silence() {
        let mut x = sine(48000.0, 1000.0, 10f64.powf(-23.0 / 20.0), 5.0, 0.0);
        x.extend(std::iter::repeat(0.0).take(5 * 48000));
        let expected = -23.0 + 10.0 * ((47.0 + 0.75 + 0.5 + 0.25) / 50.0f64).log10();
        assert!((lufs(&x, &x, 48000.0).0 - expected).abs() < 0.05);
    }

    #[test]
    fn mono_loss_in_phase_and_cancelling() {
        let t = sine(48000.0, 1000.0, 0.3, 5.0, 0.0);
        let neg: Vec<f64> = t.iter().map(|v| -v).collect();
        let get = |l: &[f64], r: &[f64], k: &str| mono_loss(l, r, 48000.0).0.iter().find(|x| x.0 == k).map(|x| x.1).unwrap();
        assert!(get(&t, &t, "mid").abs() < 0.1);
        assert!(get(&t, &neg, "mid") < -40.0);
    }

    #[test]
    fn uncorrelated_noise_loses_three_db() {
        let mut s = 12345u64;
        let mut rnd = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((s >> 33) as f64 / (1u64 << 31) as f64) - 0.5
        };
        let a: Vec<f64> = (0..5 * 48000).map(|_| rnd()).collect();
        let b: Vec<f64> = (0..5 * 48000).map(|_| rnd()).collect();
        let p = mono_loss(&a, &b, 48000.0).0.iter().find(|x| x.0 == "presence").unwrap().1;
        assert!((p + 3.0).abs() < 0.3, "{p}");
    }

    #[test]
    fn low_width_sees_wide_bass_only() {
        let bass = sine(48000.0, 60.0, 0.3, 5.0, 0.0);
        let wide = sine(48000.0, 60.0, 0.3, 5.0, std::f64::consts::FRAC_PI_2);
        assert!(mono_loss(&bass, &bass, 48000.0).1.unwrap() < -60.0);
        assert!(mono_loss(&bass, &wide, 48000.0).1.unwrap() > -1.0);
    }

    #[test]
    fn corridor_flags_one_band_and_is_level_independent() {
        let shape: Vec<f64> = (0..8).map(|i| -10.0 - i as f64).collect();
        let refs: Vec<Vec<f64>> = [0.0, -8.0, 5.0].iter().map(|o| shape.iter().map(|v| v + o).collect()).collect();
        let cor = target_corridor(&refs).unwrap();
        assert!(cor.iter().all(|c| c.1 == 1.0));
        let mut mix: Vec<f64> = shape.iter().map(|v| v - 20.0).collect();
        mix[1] += 6.0;
        let v = target_verdict(&mix, &cor);
        assert!((v[1] - 4.25).abs() < 1e-9, "{v:?}");
        assert!(v.iter().enumerate().all(|(i, x)| i == 1 || *x == 0.0));
    }

    /// Golden values produced by `tools/mixcheck.py` (Python) on the same signal; tolerance 0.1 dB / 0.1 LU.
    #[test]
    fn agrees_with_the_python_tool() {
        let sr = 44100.0;
        let mk = |a: [(f64, f64, f64); 3]| -> Vec<f64> {
            let n = (6.0 * sr) as usize;
            (0..n).map(|i| a.iter().map(|(f, amp, p)| amp * (2.0 * std::f64::consts::PI * f * i as f64 / sr + p).sin()).sum()).collect()
        };
        let l = mk([(100.0, 0.3, 0.0), (3000.0, 0.1, 0.0), (9000.0, 0.05, 0.3)]);
        let r = mk([(100.0, 0.25, 0.5), (3000.0, 0.1, 1.0), (9000.0, 0.05, 2.0)]);
        let bands = band_energy(&mono_of(&l, &r), sr);
        for (i, want) in [(0, -49.667), (1, 0.0), (2, -23.977), (5, -9.648), (6, -18.144)] {
            assert!((bands[i] - want).abs() < 0.1, "band {i}: {} vs {want}", bands[i]);
        }
        let (li, ls) = lufs(&l, &r, sr);
        assert!((li + 11.188).abs() < 0.1 && (ls + 11.188).abs() < 0.1, "{li} {ls}");
        let (loss, w) = mono_loss(&l, &r, sr);
        for (k, want) in [("sub", -0.307), ("mid", -0.647), ("presence", -1.134), ("air", -3.609)] {
            let got = loss.iter().find(|x| x.0 == k).unwrap().1;
            assert!((got - want).abs() < 0.1, "{k}: {got} vs {want}");
        }
        assert!((w.unwrap() + 11.342).abs() < 0.1);
        assert!((correlation(&l, &r) - 0.80863).abs() < 1e-3);
    }
}
