//! pitch-bench — put our two pitch trackers side by side.
//!
//! Both trackers are real, shipped code paths (`synth_core::pitch::
//! YinPitchTracker` and `synth_core::swiftf0::SwiftF0Tracker`), driven
//! sample-by-sample exactly as a plugin drives them. On a synthetic probe the
//! ground truth is known, so the table is accuracy in cents and octave-error
//! rate; on a real file there is no truth, so the two are compared against
//! each other and the disagreements are what you listen to.
//!
//! ```text
//! pitch-bench                       # synthetic suite (steady, vibrato, tremolo, sweep, noisy)
//! pitch-bench voice.wav             # a real take: agreement + CPU
//! pitch-bench voice.wav --csv out.csv
//! ```

use std::time::Instant;
use superduper_synth_core::pitch::YinPitchTracker;
use superduper_synth_core::swiftf0::SwiftF0Tracker;

const SR: f32 = 48_000.0;
/// Gate for SwiftF0's own confidence output.
const CONF_GATE: f32 = 0.5;

struct Run {
    hz: Vec<f32>,
    at: Vec<f32>, // seconds
    secs: f64,
    cpu_ms: f64,
}

impl Run {
    fn rtf(&self) -> f64 {
        self.secs / (self.cpu_ms / 1000.0)
    }
    fn core_pct(&self) -> f64 {
        self.cpu_ms / (self.secs * 1000.0) * 100.0
    }
}

fn run_yin(sig: &[f32], sr: f32) -> Run {
    let mut tr = YinPitchTracker::new(sr, 70.0, 1000.0, 1536, 256, 150.0);
    let (mut hz, mut at) = (Vec::new(), Vec::new());
    let t0 = Instant::now();
    for (i, &x) in sig.iter().enumerate() {
        if tr.push(x) {
            hz.push(tr.current_hz());
            at.push(i as f32 / sr);
        }
    }
    let cpu_ms = t0.elapsed().as_secs_f64() * 1000.0;
    Run { hz, at, secs: sig.len() as f64 / sr as f64, cpu_ms }
}

fn run_swift(sig: &[f32], sr: f32) -> Run {
    let mut tr = SwiftF0Tracker::new(sr, 150.0);
    let (mut hz, mut at) = (Vec::new(), Vec::new());
    let t0 = Instant::now();
    for (i, &x) in sig.iter().enumerate() {
        if tr.push(x) {
            // Unvoiced frames report NaN so downstream stats can skip them;
            // a plugin would hold its last value instead.
            hz.push(if tr.confidence() >= CONF_GATE { tr.current_hz() } else { f32::NAN });
            at.push(i as f32 / sr);
        }
    }
    let cpu_ms = t0.elapsed().as_secs_f64() * 1000.0;
    Run { hz, at, secs: sig.len() as f64 / sr as f64, cpu_ms }
}

fn cents(a: f32, b: f32) -> f32 {
    (a / b).log2() * 1200.0
}

fn pct(v: &mut Vec<f32>, p: f32) -> f32 {
    if v.is_empty() {
        return f32::NAN;
    }
    v.sort_by(f32::total_cmp);
    v[((v.len() - 1) as f32 * p) as usize]
}

/// Accuracy against a known truth curve, skipping the first `skip_s` seconds
/// (both trackers need their window to fill).
fn score(run: &Run, truth: impl Fn(f32) -> f32, skip_s: f32) -> (f32, f32, f32, usize) {
    let mut err = Vec::new();
    let mut octave = 0usize;
    let mut used = 0usize;
    for (&t, &h) in run.at.iter().zip(&run.hz) {
        if t < skip_s || !h.is_finite() {
            continue;
        }
        used += 1;
        let c = cents(h, truth(t));
        if c.abs() > 600.0 {
            octave += 1;
        }
        err.push(c.abs());
    }
    let mut e = err;
    let med = pct(&mut e, 0.5);
    let p95 = pct(&mut e, 0.95);
    (med, p95, octave as f32 / used.max(1) as f32 * 100.0, used)
}

struct Probe {
    name: &'static str,
    /// True f0 at time t.
    truth: fn(f32) -> f32,
    /// Amplitude envelope at time t.
    amp: fn(f32) -> f32,
    /// Extra broadband noise, linear.
    noise: f32,
    /// Harmonic richness: number of harmonics summed at 1/k.
    harmonics: usize,
}

const PROBES: &[Probe] = &[
    Probe { name: "steady 220",      truth: |_| 220.0, amp: |_| 0.7, noise: 0.0,  harmonics: 6 },
    Probe { name: "vibrato ±50c",    truth: |t| 220.0 * (2f32).powf(0.5 / 12.0 * (t * 5.5 * std::f32::consts::TAU).sin()),
            amp: |_| 0.7, noise: 0.0, harmonics: 6 },
    Probe { name: "tremolo 6 Hz",    truth: |_| 200.0,
            amp: |t| 0.15 + 0.6 * (0.5 + 0.5 * (t * 6.0 * std::f32::consts::TAU).sin()),
            noise: 0.0, harmonics: 8 },
    Probe { name: "sweep 110→440",   truth: |t| 110.0 * (4f32).powf((t / 3.0).min(1.0)), amp: |_| 0.7, noise: 0.0, harmonics: 5 },
    Probe { name: "noisy 165 (-6dB)", truth: |_| 165.0, amp: |_| 0.5, noise: 0.25, harmonics: 7 },
    Probe { name: "low 82 (bass)",   truth: |_| 82.4, amp: |_| 0.7, noise: 0.02, harmonics: 10 },
];

fn synth(p: &Probe, secs: f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    let mut out = Vec::with_capacity(n);
    let mut phase = 0.0f32;
    let mut rng = 0x2545_F491_4F6C_DD1Du64;
    for i in 0..n {
        let t = i as f32 / SR;
        let f0 = (p.truth)(t);
        phase = (phase + f0 / SR).fract();
        let mut s = 0.0;
        for k in 1..=p.harmonics {
            s += (phase * k as f32 * std::f32::consts::TAU).sin() / k as f32;
        }
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let noise = ((rng >> 40) as f32 / 8388608.0 - 1.0) * p.noise;
        out.push((p.amp)(t) * s * 0.5 + noise);
    }
    out
}

fn synthetic_suite() {
    println!("Synthetic probes — 4 s each at 48 kHz, first 0.5 s skipped\n");
    println!("{:<18} {:>22} {:>22}", "", "FFT-YIN", "SwiftF0");
    println!("{:<18} {:>10} {:>5} {:>5} {:>10} {:>5} {:>5}",
             "probe", "med cents", "p95", "oct%", "med cents", "p95", "oct%");
    let mut yin_cpu = (0.0, 0.0);
    let mut sw_cpu = (0.0, 0.0);
    for p in PROBES {
        let sig = synth(p, 4.0);
        let ry = run_yin(&sig, SR);
        let rs = run_swift(&sig, SR);
        let (ym, yp, yo, _) = score(&ry, p.truth, 0.5);
        let (sm, sp, so, _) = score(&rs, p.truth, 0.5);
        println!("{:<18} {:>10.1} {:>5.0} {:>5.1} {:>10.1} {:>5.0} {:>5.1}",
                 p.name, ym, yp, yo, sm, sp, so);
        yin_cpu = (yin_cpu.0 + ry.cpu_ms, yin_cpu.1 + ry.secs);
        sw_cpu = (sw_cpu.0 + rs.cpu_ms, sw_cpu.1 + rs.secs);
    }
    println!();
    let y = Run { hz: vec![], at: vec![], secs: yin_cpu.1, cpu_ms: yin_cpu.0 };
    let s = Run { hz: vec![], at: vec![], secs: sw_cpu.1, cpu_ms: sw_cpu.0 };
    println!("CPU over {:.0} s of audio:", y.secs);
    println!("  FFT-YIN  {:6.1}x realtime  ({:.2}% of a core)", y.rtf(), y.core_pct());
    println!("  SwiftF0  {:6.1}x realtime  ({:.2}% of a core)", s.rtf(), s.core_pct());
    println!("\nmed/p95 = |error| in cents vs the known f0; oct% = frames off by >600 cents.");
    println!("100 cents = one semitone. Under ~10 cents is inaudible on a sustained note.");
}

fn compare_file(path: &str, csv: Option<&str>) {
    let mut rd = match hound::WavReader::open(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pitch-bench: {path}: {e}");
            std::process::exit(1);
        }
    };
    let spec = rd.spec();
    let ch = spec.channels as usize;
    let sr = spec.sample_rate as f32;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => rd.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample - 1)) as f32;
            rd.samples::<i32>().map(|s| s.unwrap_or(0) as f32 * scale).collect()
        }
    };
    let mono: Vec<f32> = samples
        .chunks(ch)
        .map(|f| f.iter().sum::<f32>() / ch as f32)
        .collect();
    println!("{path}: {:.2} s, {} Hz, {} ch\n", mono.len() as f32 / sr, sr, ch);

    let ry = run_yin(&mono, sr);
    let rs = run_swift(&mono, sr);
    println!("CPU:");
    println!("  FFT-YIN  {:6.1}x realtime  ({:.2}% of a core)  {} frames", ry.rtf(), ry.core_pct(), ry.hz.len());
    println!("  SwiftF0  {:6.1}x realtime  ({:.2}% of a core)  {} frames", rs.rtf(), rs.core_pct(), rs.hz.len());

    // No ground truth on a real take: report agreement, and where they split.
    let mut diffs = Vec::new();
    let mut octave_splits = 0usize;
    let mut pairs = Vec::new();
    for (&t, &sh) in rs.at.iter().zip(&rs.hz) {
        if !sh.is_finite() {
            continue;
        }
        // Nearest YIN estimate in time.
        let j = ry.at.partition_point(|&x| x < t).min(ry.at.len().saturating_sub(1));
        let yh = ry.hz[j];
        if yh <= 0.0 {
            continue;
        }
        let c = cents(yh, sh);
        if c.abs() > 600.0 {
            octave_splits += 1;
        }
        diffs.push(c.abs());
        pairs.push((t, yh, sh, c));
    }
    if diffs.is_empty() {
        println!("\nNo voiced frames in common — nothing to compare.");
        return;
    }
    let n = diffs.len();
    let med = pct(&mut diffs.clone(), 0.5);
    let p95 = pct(&mut diffs.clone(), 0.95);
    println!("\nAgreement on {n} voiced frames:");
    println!("  median |Δ| {med:.1} cents, p95 {p95:.1} cents");
    println!("  octave-scale disagreements: {} ({:.1}%)",
             octave_splits, octave_splits as f32 / n as f32 * 100.0);
    if octave_splits > 0 {
        println!("\n  worst disagreements (listen to these spots):");
        let mut worst = pairs.clone();
        worst.sort_by(|a, b| b.3.abs().total_cmp(&a.3.abs()));
        for (t, y, s, c) in worst.iter().take(8) {
            println!("    {t:6.2}s  yin {y:7.1} Hz   swift {s:7.1} Hz   ({c:+.0} cents)");
        }
    }
    if let Some(out) = csv {
        let mut buf = String::from("time_s,yin_hz,swiftf0_hz,delta_cents\n");
        for (t, y, s, c) in &pairs {
            buf.push_str(&format!("{t:.4},{y:.3},{s:.3},{c:.2}\n"));
        }
        if let Err(e) = std::fs::write(out, buf) {
            eprintln!("csv: {e}");
        } else {
            println!("\nwrote {out}");
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        None => synthetic_suite(),
        Some("-h") | Some("--help") => {
            println!("Usage:");
            println!("  pitch-bench                     synthetic accuracy + CPU suite");
            println!("  pitch-bench <file.wav>          compare both trackers on a real take");
            println!("  pitch-bench <file.wav> --csv <out.csv>");
        }
        Some(path) => {
            let csv = args
                .iter()
                .position(|a| a == "--csv")
                .and_then(|i| args.get(i + 1))
                .map(|s| s.as_str());
            compare_file(path, csv);
        }
    }
}
