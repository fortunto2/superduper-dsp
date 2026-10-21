//! Render an SFZ instrument from a plain event list — the known-answer harness
//! against `sfizz_render`.
//!
//! cargo run --release -p superduper-synth-core --features sfz-load --example sfz_render -- \
//!     <file.sfz> <events.txt> <out.wav> [seconds]
//!
//! events.txt, one per line: `<time_s> on <key> <vel 0-127>`, `<time_s> off <key>`,
//! `<time_s> cc <n> <value 0-127>`.

use std::time::Instant;
use superduper_synth_core::sfz::{load::load_instrument, Engine, DEFAULT_VOICES};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: sfz_render <file.sfz> <events.txt> <out.wav> [seconds]");
        std::process::exit(2);
    }
    let sr = 48_000u32;
    let t0 = Instant::now();
    let inst = load_instrument(args[1].as_ref(), 8, None).unwrap_or_else(|e| {
        eprintln!("load: {e}");
        std::process::exit(1)
    });
    let bytes: usize = inst.samples.iter().map(|s| s.data.len() * 2).sum();
    eprintln!(
        "loaded {} regions, {} samples ({:.0} MB) in {:.1} s; missing {}; unsupported {:?}",
        inst.def.regions.len(), inst.samples.len(), bytes as f64 / 1e6,
        t0.elapsed().as_secs_f64(), inst.missing.len(), inst.def.unsupported
    );
    for m in inst.missing.iter().take(5) { eprintln!("  missing: {m}"); }

    let text = std::fs::read_to_string(&args[2]).expect("events");
    let mut events: Vec<(f64, String, u8, f32)> = text.lines()
        .filter_map(|l| {
            let p: Vec<&str> = l.split_whitespace().collect();
            let t = p.first()?.parse::<f64>().ok()?;
            let kind = p.get(1)?.to_string();
            let a = p.get(2)?.parse::<u8>().ok()?;
            let b = p.get(3).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
            Some((t, kind, a, b))
        })
        .collect();
    events.sort_by(|a, b| a.0.total_cmp(&b.0));
    let secs: f64 = args.get(4).and_then(|s| s.parse().ok())
        .unwrap_or_else(|| events.last().map_or(1.0, |e| e.0 + 4.0));

    let mut engine = Engine::new(sr as f32, DEFAULT_VOICES);
    engine.reset(&inst);
    let total = (secs * sr as f64) as usize;
    let (mut l, mut r) = (vec![0f32; total], vec![0f32; total]);
    let mut pos = 0usize;
    let mut ev = events.iter().peekable();
    let t_render = Instant::now();
    while pos < total {
        while let Some(e) = ev.peek() {
            if (e.0 * sr as f64) as usize > pos { break; }
            match e.1.as_str() {
                "on" => engine.note_on(&inst, e.2, e.3 / 127.0),
                "off" => engine.note_off(&inst, e.2),
                "cc" => engine.cc(&inst, e.2, e.3 / 127.0),
                _ => {}
            }
            ev.next();
        }
        let next = ev.peek().map_or(total, |e| ((e.0 * sr as f64) as usize).min(total)).max(pos + 1);
        let end = next.min(pos + 512).min(total);
        engine.render(&inst, &mut l[pos..end], &mut r[pos..end]);
        pos = end;
    }
    eprintln!("rendered {secs:.1} s in {:.2} s", t_render.elapsed().as_secs_f64());

    let spec = hound::WavSpec { channels: 2, sample_rate: sr, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(&args[3], spec).expect("wav");
    for i in 0..total {
        w.write_sample(l[i]).expect("w");
        w.write_sample(r[i]).expect("w");
    }
    w.finalize().expect("finalize");
}
