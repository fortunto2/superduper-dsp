//! The human report of `sdsp-chain verify` (the table form of tools/mixverify.py).

use serde_json::Value;

use crate::check::BANDS;
use crate::verify::{bands_touching, buried_key, buried_share, clash_key, level, levels_of, pair_band, pair_name, Args};

/// Right-aligned number, "—" when missing or not finite.
fn fmt(v: Option<f64>, w: usize, prec: usize, sign: bool) -> String {
    match v.filter(|x| x.is_finite()) {
        None => format!("{:>w$}", "—"),
        Some(x) if sign => format!("{x:+w$.prec$}"),
        Some(x) => format!("{x:w$.prec$}"),
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

struct Row<'a> {
    reference: Option<&'a Value>,
}

impl Row<'_> {
    fn print(&self, label: &str, a: Option<f64>, b: Option<f64>, unit: &str, refs: Option<(f64, f64)>, prec: usize) {
        let delta = a.zip(b).map(|(x, y)| y - x);
        let mut line = format!("  {label:28}{}{}{} {unit}", fmt(a, 9, prec, false), fmt(b, 9, prec, false), fmt(delta, 8, prec, true));
        if let (Some(_), Some((ra, rb))) = (self.reference, refs) {
            line += &format!("{}{}", fmt(Some(ra), 8, prec, true), fmt(Some(rb), 9, prec, true));
        }
        println!("{line}");
    }
}

fn print_render(r1: &Value, r2: &Value, reference: &Value, pair: Option<&Value>, question: &str) {
    let (p1, p2) = (&r1["mix"], &r2["mix"]);
    let has_ref = !reference.is_null();
    println!("\ninstrument 1 · offline render (sdsp-chain → check profile)");
    let mut head = format!("  {:28}{:>9}{:>9}{:>9}", "metric", "before", "after", "delta");
    if has_ref {
        head += &format!("{:>9}{:>9}", "vs ref", "→ ref");
    }
    println!("{head}");
    let row = Row { reference: has_ref.then_some(reference) };
    let rl = num(&reference["lufs_integrated"]);
    let lufs = |p: &Value| num(&p["lufs_integrated"]);
    let refs = lufs(p1).zip(lufs(p2)).zip(rl).map(|((a, b), r)| (a - r, b - r));
    row.print("LUFS integrated", lufs(p1), lufs(p2), "LU", refs, 1);
    row.print("LUFS short-term max", num(&p1["lufs_short_max"]), num(&p2["lufs_short_max"]), "LU", None, 1);
    row.print("crest", num(&p1["crest_db"]), num(&p2["crest_db"]), "dB", None, 1);
    row.print("correlation", num(&p1["correlation"]), num(&p2["correlation"]), "", None, 2);
    row.print("side/mid < 120 Hz", num(&p1["side_mid_low_db"]), num(&p2["side_mid_low_db"]), "dB", None, 1);
    let band = pair_band(pair);
    let exempt = if question == "masking" { bands_touching(band) } else { vec![] };
    for (lo, hi, name) in BANDS.iter() {
        let tag = format!("band {name:9}{:5}-{:<5}{}", *lo as i64, *hi as i64, if exempt.contains(name) { "*" } else { "" });
        let (a, b) = (num(&p1["bands_db"][*name]), num(&p2["bands_db"][*name]));
        let refs = a.zip(b).zip(num(&reference["bands_db"][*name])).map(|((x, y), r)| (x - r, y - r));
        row.print(&tag, a, b, "dB", refs, 1);
    }
    for (_, _, name) in BANDS.iter() {
        if let (Some(a), Some(b)) = (num(&p1["mono_loss_db"][*name]), num(&p2["mono_loss_db"][*name])) {
            if a.min(b) < -1.0 || (b - a).abs() > 0.5 {
                row.print(&format!("mono loss {name}"), Some(a), Some(b), "dB", None, 1);
            }
        }
    }
    if !exempt.is_empty() {
        println!("  (* the clash band: exempt from the band guard)");
    }
    if has_ref {
        let dist = |p: &Value| BANDS.iter().map(|(_, _, n)| (num(&p["bands_db"][*n]).unwrap_or(0.0) - num(&reference["bands_db"][*n]).unwrap_or(0.0)).abs()).sum::<f64>();
        println!("  distance to reference Σ|Δband|  {:6.1} → {:6.1} dB  (shown, not judged)", dist(p1), dist(p2));
    }
    print_stems(&row, r1, r2, band);
}

fn print_stems(row: &Row, r1: &Value, r2: &Value, band: Option<(f64, f64)>) {
    let Some(stems) = r1["stems"].as_object() else { return };
    for (name, s1) in stems {
        let s2 = &r2["stems"][name];
        if s2.is_null() {
            continue;
        }
        let short: String = name.chars().take(23).collect();
        row.print(&format!("stem {short}"), num(&s1["rms_db"]), num(&s2["rms_db"]), "dB RMS", None, 1);
        if let (Some(_), Some((lo, hi))) = (s1.get("band_db"), band) {
            row.print(&format!("  @ {lo:.0}-{hi:.0} Hz"), num(&s1["band_db"]), num(&s2["band_db"]), "dB", None, 1);
        }
    }
}

fn clash_table(b1: &Value, b2: &Value, pair: Option<&Value>) {
    let list = |b: &Value| b["clashes"].as_array().cloned().unwrap_or_default();
    let (c1, c2) = (list(b1), list(b2));
    println!("  {:36}{:>14}{:>8}{:>8}{:>8}", "clash", "band", "before", "after", "delta");
    let mut keys: Vec<(crate::verify::ClashKey, Value)> = vec![];
    for c in c1.iter().chain(&c2) {
        if !keys.iter().any(|(k, _)| *k == clash_key(c)) {
            keys.push((clash_key(c), c.clone()));
        }
    }
    let share = |cs: &[Value], k: &crate::verify::ClashKey| cs.iter().find(|c| clash_key(c) == *k).and_then(|c| num(&c["share"])).unwrap_or(0.0);
    keys.sort_by(|a, b| share(&c1, &b.0).max(share(&c2, &b.0)).partial_cmp(&share(&c1, &a.0).max(share(&c2, &a.0))).unwrap_or(std::cmp::Ordering::Equal));
    for (k, c) in &keys {
        let (s1, s2) = (share(&c1, k), share(&c2, k));
        let mark = if pair.is_some_and(|p| clash_key(p) == *k) { "  ← the question" } else { "" };
        let name: String = pair_name(c).chars().take(36).collect();
        let band = format!("{:6.0}-{:<7.0}", num(&c["low_hz"]).unwrap_or(0.0), num(&c["high_hz"]).unwrap_or(0.0));
        println!("  {name:36}{band}{:7.0} %{:7.0} %{:+7.0} pt{mark}", s1 * 100.0, s2 * 100.0, (s2 - s1) * 100.0);
    }
    if keys.is_empty() {
        println!("  (no clashes in either run)");
    }
}

fn buried_table(b1: &Value, b2: &Value) {
    let mut seen: Vec<(crate::verify::BuriedKey, Value)> = vec![];
    for b in b1["buried"].as_array().into_iter().flatten().chain(b2["buried"].as_array().into_iter().flatten()) {
        if !seen.iter().any(|(k, _)| *k == buried_key(b)) {
            seen.push((buried_key(b), b.clone()));
        }
    }
    if seen.is_empty() {
        return;
    }
    println!("  {:36}{:>14}{:>8}{:>8}", "buried", "band", "before", "after");
    for (key, b) in &seen {
        let name: String = format!("{} under {}", b["quiet"].as_str().unwrap_or(""), b["under"].as_str().unwrap_or("")).chars().take(36).collect();
        let band = format!("{:6.0}-{:<7.0}", num(&b["low_hz"]).unwrap_or(0.0), num(&b["high_hz"]).unwrap_or(0.0));
        println!("  {name:36}{band}{:7.0} %{:7.0} %", buried_share(b1, key) * 100.0, buried_share(b2, key) * 100.0);
    }
}

fn print_bus(b1: &Value, b2: &Value, labels: &Value, pair: Option<&Value>) {
    println!("\ninstrument 2 · bus (mix-log.jsonl: '{}' → '{}')", labels[0].as_str().unwrap_or(""), labels[1].as_str().unwrap_or(""));
    clash_table(b1, b2, pair);
    buried_table(b1, b2);
    let (l1, l2) = (levels_of(b1), levels_of(b2));
    println!("  {:36}{:>14}{:>8}{:>8}{:>8}", "level", "", "before", "after", "delta");
    let mut names: Vec<&String> = l1.iter().chain(&l2).map(|(n, _)| n).collect();
    names.sort();
    names.dedup();
    for n in names {
        if let (Some(a), Some(b)) = (level(&l1, n), level(&l2, n)) {
            if a.max(b) > -100.0 {
                let short: String = n.chars().take(36).collect();
                println!("  {short:36}{:14}{a:8.1}{b:8.1}{:+8.1} dB", "", b - a);
            }
        }
    }
}

pub fn print_report(args: &Args, res: &Value) {
    let name = |p: &str| std::path::Path::new(p).file_name().and_then(|n| n.to_str()).unwrap_or(p).to_string();
    let sec = match (res["section"][0].as_f64(), res["section"][1].as_f64()) {
        (Some(a), Some(b)) => format!("   section {a:?}–{b:?} s"),
        _ => String::new(),
    };
    println!("verify   {} → {}   question: {}{sec}", name(&args.before), name(&args.after), args.question);
    let pair = Some(&res["pair"]).filter(|p| !p.is_null());
    if let (true, Some(p)) = (args.question == "masking", pair) {
        let band = match (num(&p["low_hz"]), num(&p["high_hz"])) {
            (Some(lo), Some(hi)) => format!(" at {lo:.0}-{hi:.0} Hz"),
            _ => String::new(),
        };
        let hint = if args.pair.is_some() { "" } else { "   (worst clash in the before run; name one with --pair)" };
        println!("pair: {}{band}{hint}", pair_name(p));
    }
    if args.question == "balance" {
        println!("track: {}", res["track"].as_str().unwrap_or("None"));
    }
    if !res["render"]["before"].is_null() {
        print_render(&res["render"]["before"], &res["render"]["after"], &res["reference"], pair, &args.question);
    } else {
        println!("\ninstrument 1 · offline render: MISSING — {}", res["render"]["note"].as_str().unwrap_or("none"));
    }
    if !res["bus"]["before"].is_null() && !res["bus"]["after"].is_null() {
        print_bus(&res["bus"]["before"], &res["bus"]["after"], &res["labels"], pair);
    } else {
        println!("\ninstrument 2 · bus: MISSING — {}", res["bus"]["note"].as_str().unwrap_or("none"));
    }
    println!("\nVERDICT: {}", res["verdict"].as_str().unwrap_or("UNKNOWN"));
    for r in res["reasons"].as_array().into_iter().flatten() {
        println!("  {}", r.as_str().unwrap_or(""));
    }
}
