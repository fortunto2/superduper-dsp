//! Measure the sound-based drum classifier against labelled packs, and print
//! what a full library scan finds.
//!
//! cargo run --release -p superduper-synth-core --features sfz-load --example drum_classify -- <labelled_dir>...
//!
//! The label for each file comes from its NAME (folders included) — the
//! audio classifier never sees it — so the confusion matrix is an honest
//! measure of how it does on files whose names say nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use superduper_synth_core::drumlib::*;
use superduper_synth_core::sfz::load::decode_file;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() { walk(&p, out); }
        else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")) { out.push(p); }
    }
}

fn main() {
    let dirs: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    for dir in &dirs {
        let mut files = Vec::new();
        walk(dir, &mut files);
        files.retain(|p| !looks_like_loop(p));
        let mut confusion: BTreeMap<(DrumClass, DrumClass), usize> = BTreeMap::new();
        let (mut ok, mut total, mut too_long) = (0, 0, 0);
        let mut features: Vec<(DrumClass, AudioFeatures)> = Vec::new();
        for p in &files {
            let Some(truth) = classify_name(p) else { continue };
            let Ok(buf) = decode_file(p) else { continue };
            let f = audio_features(&mono_of(&buf), buf.sample_rate);
            if f.duration_s > MAX_ONESHOT_S { too_long += 1; continue; }
            let guess = classify_audio(&f);
            *confusion.entry((truth, guess)).or_default() += 1;
            total += 1;
            if guess == truth || guess.pad() == truth.pad() { ok += 1; }
            features.push((truth, f));
        }
        println!("\n== {} — {} labelled one-shots ({} over {MAX_ONESHOT_S} s skipped)", dir.display(), total, too_long);
        println!("right pad: {ok}/{total} = {:.0}%", 100.0 * ok as f64 / total.max(1) as f64);
        print!("{:>11}", "truth\\guess");
        for g in DrumClass::ALL { print!("{:>8}", &g.label()[..g.label().len().min(7)]); }
        println!();
        for t in DrumClass::ALL {
            if !confusion.keys().any(|k| k.0 == t) { continue; }
            print!("{:>11}", t.label());
            for g in DrumClass::ALL { print!("{:>8}", confusion.get(&(t, g)).copied().unwrap_or(0)); }
            println!();
        }
        // Per-class medians — what the thresholds are set from.
        println!("{:>11}{:>8}{:>8}{:>9}{:>8}", "class", "dur", "low", "centroid", "flat");
        for t in DrumClass::ALL {
            let mut v: Vec<&AudioFeatures> = features.iter().filter(|x| x.0 == t).map(|x| &x.1).collect();
            if v.is_empty() { continue; }
            let med = |f: &dyn Fn(&AudioFeatures) -> f32, v: &mut Vec<&AudioFeatures>| {
                v.sort_by(|a, b| f(a).total_cmp(&f(b)));
                f(v[v.len() / 2])
            };
            println!("{:>11}{:>8.2}{:>8.2}{:>9.0}{:>8.3}", t.label(),
                med(&|f| f.duration_s, &mut v), med(&|f| f.low_ratio, &mut v),
                med(&|f| f.centroid_hz, &mut v), med(&|f| f.flatness, &mut v));
        }
    }

    let t0 = std::time::Instant::now();
    let lib = scan_library(&default_roots(), 8);
    let mut by: BTreeMap<(DrumClass, ClassSource), usize> = BTreeMap::new();
    for s in &lib { *by.entry((s.class, s.source)).or_default() += 1; }
    if let Some(dump) = std::env::var_os("DRUM_DUMP") {
        let rows: Vec<String> = lib.iter().map(|s| format!("{}\t{:?}\t{}", s.class.label(), s.source, s.path.display())).collect();
        let _ = std::fs::write(dump, rows.join("\n"));
    }
    println!("\n== full scan of {:?}: {} one-shots in {:.1} s", default_roots(), lib.len(), t0.elapsed().as_secs_f64());
    for c in DrumClass::ALL {
        let n = by.get(&(c, ClassSource::Name)).copied().unwrap_or(0);
        let a = by.get(&(c, ClassSource::Audio)).copied().unwrap_or(0);
        println!("{:>11}: {n} by name, {a} by sound (pad {})", c.label(), c.pad());
    }
}
