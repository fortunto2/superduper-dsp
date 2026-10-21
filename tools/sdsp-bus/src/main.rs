//! sdsp-bus — what every running SuperDuper plugin hears, right now.
//!
//!     sdsp-bus            one table
//!     sdsp-bus --watch    refresh twice a second
//!     sdsp-bus --json     machine-readable (agents)
//!
//! Each row is one plugin instance: its track, level, peak, a 32-band
//! spectrum as a bar strip (20 Hz on the left, 20 kHz on the right) and the
//! key it detected, if it detects one.
use superduper_dsp_sdk::bus::{band_edges, Reader, BANDS};

const KEYS: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

fn strip(bands: &[f32; BANDS]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    bands.iter().map(|&db| {
        let t = ((db + 90.0) / 80.0).clamp(0.0, 0.999);
        if db < -89.0 { ' ' } else { BARS[(t * 8.0) as usize] }
    }).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let watch = args.iter().any(|a| a == "--watch");
    let json = args.iter().any(|a| a == "--json");
    let Some(r) = Reader::open() else {
        eprintln!("cannot open {}", superduper_dsp_sdk::bus::bus_path().display());
        std::process::exit(1);
    };
    loop {
        let mut all = r.read_all();
        all.sort_by(|a, b| (&a.track, &a.plugin).cmp(&(&b.track, &b.plugin)));
        if json {
            let rows: Vec<_> = all.iter().map(|s| serde_json::json!({
                "track": s.track, "plugin": s.plugin, "rms_db": s.rms_db, "peak_db": s.peak_db,
                "key": s.key.map(|k| KEYS[k as usize]), "key_confidence": s.key_confidence,
                "bands_db": s.bands_db.to_vec(), "band_edges_hz": band_edges().to_vec(),
                "age_ms": s.age_ms, "pid": s.pid,
            })).collect();
            println!("{}", serde_json::to_string_pretty(&rows).unwrap_or_default());
        } else {
            if watch { print!("\x1b[2J\x1b[H"); }
            println!("{:<22} {:<24} {:>7} {:>7}  {:<32}  key", "track", "plugin", "rms", "peak", "20 Hz ─ spectrum ─ 20 kHz");
            for s in &all {
                let key = s.key.map_or("–".to_string(), |k| format!("{} {:.0}%", KEYS[k as usize], s.key_confidence * 100.0));
                let plugin = s.plugin.trim_start_matches("SuperDuper ");
                println!("{:<22} {:<24} {:>7.1} {:>7.1}  {}  {key}", if s.track.is_empty() { "(no track name)" } else { &s.track }, plugin, s.rms_db, s.peak_db, strip(&s.bands_db));
            }
            if all.is_empty() { println!("(nothing on the bus — no SuperDuper plugin is running)"); }
        }
        if !watch { break; }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}
