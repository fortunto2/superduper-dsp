use superduper_synth_core::nam::{load_from_json, NamModel};
use std::time::Instant;

fn bench_one(name: &str, path: &str) {
    let text = std::fs::read_to_string(path).expect("read");
    let file = load_from_json(&text).expect("parse");
    let mut model = NamModel::from_nam_file(&file).expect("build");
    let n = 48_000;
    let input: Vec<f32> = (0..n).map(|i| (i as f32 * 0.1).sin() * 0.3).collect();
    let mut output = vec![0.0; n];

    // Warm up cache
    for &x in &input[..1024] { model.process(x); }
    model.reset();

    let t0 = Instant::now();
    for (i, &x) in input.iter().enumerate() { output[i] = model.process(x); }
    let dt = t0.elapsed();
    let ms = dt.as_secs_f32() * 1000.0;
    let cpu_pct = ms / 1000.0 * 100.0;  // ms per 1000ms of audio = %CPU
    println!("{:30} {:>5} params  {:>8.2} ms / 1s audio  ≈ {:.2}% CPU per mono channel",
             name, model.param_count(), ms, cpu_pct);
}

fn main() {
    let dir = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".superduper-dsp/nam");
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("nam") {
            let name = p.file_stem().and_then(|s| s.to_str()).unwrap_or("?").to_string();
            let path = p.to_string_lossy().to_string();
            // skip unsupported
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(file) = load_from_json(&text) {
                    if NamModel::from_nam_file(&file).is_ok() {
                        bench_one(&name, &path);
                    }
                }
            }
        }
    }
}
