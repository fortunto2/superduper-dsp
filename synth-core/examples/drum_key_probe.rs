//! How well can a drum's key be read from its sound? Ground truth: KSHMR
//! names (`Big_Kick_02_D`). Prints exact / ±1 semitone hit rates for a few
//! analysis windows.
use std::path::{Path, PathBuf};
use superduper_synth_core::drumlib::{classify_name, key_from_name, looks_like_loop, mono_of, DrumClass};
use superduper_synth_core::pitch::detect_pitch_hz;
use superduper_synth_core::sfz::load::decode_file;

fn walk(d: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(d) { for e in rd.flatten() { let p = e.path(); if p.is_dir() { walk(&p, out) } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("wav")) { out.push(p) } } }
}

fn pc(hz: f32) -> i32 { (((12.0 * (hz / 440.0).log2()).round() as i32 + 9) % 12 + 12) % 12 }

/// Strongest spectral peak 30-600 Hz, parabolic-interpolated, zero-padded FFT.
fn fft_peak(x: &[f32], sr: f32) -> Option<f32> {
    if x.len() < 1024 { return None; }
    let n = 32768usize;
    let mut buf = vec![0f32; n];
    for (i, v) in x.iter().enumerate().take(n) {
        let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (x.len() - 1) as f32).cos();
        buf[i] = v * w;
    }
    let mut pl = realfft::RealFftPlanner::<f32>::new();
    let f = pl.plan_fft_forward(n);
    let mut sp = f.make_output_vec();
    f.process(&mut buf, &mut sp).ok()?;
    let bin = sr / n as f32;
    let (lo, hi) = ((30.0 / bin) as usize, (600.0 / bin) as usize);
    let m: Vec<f32> = sp.iter().map(|c| c.norm()).collect();
    let k = (lo..hi).max_by(|&a, &b| m[a].total_cmp(&m[b]))?;
    if m[k] < 1e-6 { return None; }
    let (a, b, c) = (m[k - 1].ln(), m[k].ln(), m[k + 1].ln());
    let d = 0.5 * (a - c) / (a - 2.0 * b + c);
    Some((k as f32 + d) * bin)
}

fn main() {
    let root = PathBuf::from(std::env::args().nth(1).expect("dir"));
    let mut files = Vec::new();
    walk(&root, &mut files);
    // (name, skip_ms, window_ms)
    let methods: [(&str, f32, f32); 6] = [("fft tail 120+250", -2.0, 0.0), ("fft tail 60+400", -3.0, 0.0), ("loudest (current)", -1.0, 0.0), ("tail 40ms+85", 40.0, 85.0), ("tail 80ms+170", 80.0, 170.0), ("tail 120ms+250", 120.0, 250.0)];
    let mut stats = vec![(0usize, 0usize, 0usize); methods.len()];
    let mut n = 0;
    for p in files.iter().filter(|p| !looks_like_loop(p)) {
        let Some(DrumClass::Kick | DrumClass::Tom) = classify_name(p) else { continue };
        let Some(truth) = key_from_name(p) else { continue };
        let Ok(b) = decode_file(p) else { continue };
        let mono = mono_of(&b);
        let sr = b.sample_rate;
        n += 1;
        let peak_i = mono.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).map_or(0, |x| x.0);
        for (mi, &(_, skip, win)) in methods.iter().enumerate() {
            let est = if skip == -2.0 || skip == -3.0 {
                let (a, w) = if skip == -2.0 { (0.12, 0.25) } else { (0.06, 0.40) };
                let s0 = peak_i + (a * sr) as usize;
                fft_peak(&mono[s0.min(mono.len())..(s0 + (w * sr) as usize).min(mono.len())], sr)
            } else if skip < 0.0 { detect_pitch_hz(&mono, sr as u32) } else {
                let s = peak_i + (skip / 1000.0 * sr) as usize;
                let e = (s + (win / 1000.0 * sr) as usize).min(mono.len());
                if e <= s + 2048 { None } else { detect_pitch_hz(&mono[s..e], sr as u32) }
            };
            match est {
                Some(hz) => {
                    let d = ((pc(hz) - truth as i32) % 12 + 12) % 12;
                    let d = d.min(12 - d);
                    if d == 0 { stats[mi].0 += 1 }
                    if d <= 1 { stats[mi].1 += 1 }
                }
                None => stats[mi].2 += 1,
            }
        }
    }
    println!("{n} kicks/toms with a key in the name");
    for (mi, (name, ..)) in methods.iter().enumerate() {
        let (ex, near, none) = stats[mi];
        println!("{name:>20}: exact {:.0}%  ±1 st {:.0}%  no estimate {}", 100.0 * ex as f64 / n as f64, 100.0 * near as f64 / n as f64, none);
    }
}
