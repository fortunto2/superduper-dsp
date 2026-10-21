//! Known-answer tests on the Morph engine (no host).
//!
//! The synthetic "voice" is a harmonic tone whose f0 sits exactly on a bin of
//! the 32768-point measurement FFT (150 bins = 219.73 Hz at 48 kHz), so every
//! harmonic is measured without scalloping. Profiles are compared after
//! removing their mean dB over the compared harmonics: Morph is level-matched
//! per frame, so only the shape is the claim.

use superduper_morph::engine::{MorphEngine, Settings, Source, LATENCY};
use superduper_morph::targets::{kubyz_model, ProfileJson};
use superduper_synth_core::analysis::magnitude_spectrum_db;
use superduper_synth_core::timbre::ModelTarget;

const SR: f32 = 48_000.0;
const MEAS: usize = 32_768;
const BIN: f32 = SR / MEAS as f32;
const F0: f32 = 150.0 * BIN; // 219.73 Hz
const F0_SC: f32 = 225.0 * BIN; // 329.59 Hz — a fifth above the voice

fn tone(f0: f32, amps_db: &[f32], secs: f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    let amps: Vec<f32> = amps_db.iter().map(|d| 10f32.powf(d / 20.0)).collect();
    let norm = 0.5 / amps.iter().sum::<f32>().max(1e-6);
    (0..n)
        .map(|i| {
            let t = i as f64 / SR as f64;
            amps.iter()
                .enumerate()
                .map(|(k, a)| a * (std::f64::consts::TAU * f0 as f64 * (k + 1) as f64 * t).sin() as f32)
                .sum::<f32>()
                * norm
        })
        .collect()
}

/// The voice: 20 harmonics falling 1/k (−6 dB/oct), like a bright vowel.
fn voice_db() -> Vec<f32> {
    (1..=20).map(|k| -20.0 * (k as f32).log10()).collect()
}

fn run(main: &[f32], sc: &[f32], s: &Settings, model: ModelTarget, captured: Option<superduper_synth_core::timbre::TimbreProfile>) -> (Vec<f32>, MorphEngine) {
    let mut e = MorphEngine::new(SR, model);
    if let Some(c) = captured {
        e.captured = c;
    }
    let out = main
        .iter()
        .enumerate()
        .map(|(i, &x)| {
            let y = sc.get(i).copied().unwrap_or(0.0);
            e.process(x, x, y, y, s).0
        })
        .collect();
    (out, e)
}

/// dB level of harmonics 1..=k_max of `f0` in the last MEAS samples.
fn harmonics(x: &[f32], f0: f32, k_max: usize) -> Vec<f32> {
    let spec = magnitude_spectrum_db(&x[x.len() - MEAS..]);
    (1..=k_max).map(|k| spec[(k as f32 * f0 / BIN).round() as usize]).collect()
}

fn demean(x: &[f32]) -> Vec<f32> {
    let m = x.iter().sum::<f32>() / x.len() as f32;
    x.iter().map(|v| v - m).collect()
}

fn settings(morph: f32, source: Source) -> Settings {
    Settings { morph, source, breath: 0.3, ..Settings::default() }
}

#[test]
fn morph_zero_is_the_delayed_dry_bit_for_bit() {
    let main = tone(F0, &voice_db(), 1.0);
    let mut noise = 0x1234_5678u32;
    let sc: Vec<f32> = (0..main.len())
        .map(|_| {
            noise ^= noise << 13;
            noise ^= noise >> 17;
            noise ^= noise << 5;
            noise as f32 / u32::MAX as f32 - 0.5
        })
        .collect();
    let (out, _) = run(&main, &sc, &settings(0.0, Source::Sidechain), kubyz_model(1), None);
    let worst = (LATENCY..main.len()).map(|i| (out[i] - main[i - LATENCY]).abs()).fold(0.0, f32::max);
    assert_eq!(worst, 0.0, "Morph 0 must be exactly the input delayed by {LATENCY}");
    assert!(out[..LATENCY].iter().all(|&y| y == 0.0));
}

#[test]
fn morphing_toward_itself_is_transparent_and_aligned() {
    // Target = the voice itself: every gain is 0 dB, so the wet path must
    // reconstruct the input at exactly the reported latency.
    let main = tone(F0, &voice_db(), 1.5);
    let s = Settings { morph: 1.0, breath: 1.0, source: Source::Sidechain, ..Settings::default() };
    let (out, _) = run(&main, &main, &s, kubyz_model(1), None);
    let skip = 24_000;
    let (mut err, mut sig) = (0.0f64, 0.0f64);
    for i in skip..main.len() {
        let d = (out[i] - main[i - LATENCY]) as f64;
        err += d * d;
        sig += (main[i - LATENCY] as f64).powi(2);
    }
    let db = 10.0 * (err / sig).log10();
    println!("self-morph residual {db:.1} dB");
    assert!(db < -40.0, "self-morph residual {db:.1} dB");
}

#[test]
fn morph_one_lands_on_the_kubyz_profile_and_half_is_halfway() {
    let model = kubyz_model(1); // Bashkir
    let main = tone(F0, &voice_db(), 2.0);
    let k_max = 16;
    let (o1, e) = run(&main, &[], &settings(1.0, Source::Model), model, None);
    let (oh, _) = run(&main, &[], &settings(0.5, Source::Model), model, None);
    println!("frame f0 {:.2} voiced {} mode {:?}", e.frame.f0, e.frame.voiced, e.mode);
    let input = harmonics(&main, F0, k_max);
    let full = harmonics(&o1, F0, k_max);
    let half = harmonics(&oh, F0, k_max);
    let target: Vec<f32> = (1..=k_max).map(|k| model.source_db(k) + model.filter_db(k as f32 * F0)).collect();
    // Compare where the target is within 30 dB of its loudest harmonic:
    // below that the +30 dB gain ceiling (by design) stops the climb.
    let tmax = target.iter().copied().fold(f32::MIN, f32::max);
    let idx: Vec<usize> = (0..k_max).filter(|&k| target[k] > tmax - 30.0).collect();
    let pick = |v: &[f32]| demean(&idx.iter().map(|&k| v[k]).collect::<Vec<_>>());
    let (t, f, h, i) = (pick(&target), pick(&full), pick(&half), pick(&input));
    for (j, &k) in idx.iter().enumerate() {
        println!("h{:2}: in {:6.1} target {:6.1} morph1 {:6.1} morph½ {:6.1} (want {:6.1})", k + 1, i[j], t[j], f[j], h[j], 0.5 * (i[j] + t[j]));
    }
    for (j, &k) in idx.iter().enumerate() {
        assert!((f[j] - t[j]).abs() < 2.0, "h{} morph 1: {:.1} vs target {:.1}", k + 1, f[j], t[j]);
        let want = 0.5 * (i[j] + t[j]);
        assert!((h[j] - want).abs() < 2.0, "h{} morph ½: {:.1} vs {:.1}", k + 1, h[j], want);
    }
    // Level-matched: Morph does not move loudness by more than a dB or two.
    let rms = |x: &[f32]| (x[x.len() - MEAS..].iter().map(|v| v * v).sum::<f32>() / MEAS as f32).sqrt();
    let dl = 20.0 * (rms(&o1) / rms(&main)).log10();
    assert!(dl.abs() < 2.0, "level moved {dl:.1} dB");
}

/// A sidechain target at a different pitch with a known (odd-heavy) profile.
fn sc_profile_db() -> Vec<f32> {
    vec![0.0, -14.0, -3.0, -20.0, -6.0, -24.0, -9.0, -28.0, -12.0, -16.0, -20.0, -25.0]
}

fn assert_profile_at_voice_pitch(out: &[f32], label: &str) {
    let want = demean(&sc_profile_db());
    let got = demean(&harmonics(out, F0, want.len()));
    for k in 0..want.len() {
        println!("{label} h{:2}: got {:6.1} want {:6.1}", k + 1, got[k], want[k]);
    }
    for k in 0..want.len() {
        assert!((got[k] - want[k]).abs() < 2.0, "{label} h{}: {:.1} vs {:.1}", k + 1, got[k], want[k]);
    }
}

#[test]
fn sidechain_profile_transfers_to_the_voice_pitch_and_capture_round_trips() {
    let main = tone(F0, &voice_db(), 2.0);
    let sc = tone(F0_SC, &sc_profile_db(), 2.0);
    let (out, mut e) = run(&main, &sc, &settings(1.0, Source::Sidechain), kubyz_model(1), None);
    println!("sidechain f0 {:.2} voiced {}", e.frame_sc.f0, e.sc_voiced());
    assert!((e.frame_sc.f0 - F0_SC).abs() < 2.0, "sidechain pitch {:.2}", e.frame_sc.f0);
    assert_profile_at_voice_pitch(&out, "sidechain");

    // Capture → JSON → back, then play with the sidechain silent.
    assert!(e.capture());
    let json = serde_json::to_string(&ProfileJson::from_profile(&e.captured)).unwrap();
    let back = serde_json::from_str::<ProfileJson>(&json).unwrap().to_profile().unwrap();
    assert_eq!(back, e.captured, "captured profile survives JSON exactly");
    assert!(back.harm_valid && (back.f0 - F0_SC).abs() < 2.0);
    let (out_c, _) = run(&main, &[], &settings(1.0, Source::Captured), kubyz_model(1), Some(back));
    assert_profile_at_voice_pitch(&out_c, "captured");
}

#[test]
fn a_glide_keeps_its_pitch() {
    // 200 → 300 Hz over 2 s, exponential. Gains are real, so every output
    // component must sit on a harmonic of the input's f0 at that moment.
    let secs = 2.5;
    let n = (secs * SR) as usize;
    let f_at = |t: f64| 200.0 * 1.5f64.powf((t - 0.25).clamp(0.0, 2.0) / 2.0);
    let mut ph = [0.0f64; 12];
    let main: Vec<f32> = (0..n)
        .map(|i| {
            let f = f_at(i as f64 / SR as f64);
            let mut y = 0.0;
            for (k, p) in ph.iter_mut().enumerate() {
                *p = (*p + f * (k + 1) as f64 / SR as f64).fract();
                y += (std::f64::consts::TAU * *p).sin() / (k + 1) as f64;
            }
            (0.2 * y) as f32
        })
        .collect();
    let (out, _) = run(&main, &[], &settings(1.0, Source::Model), kubyz_model(1), None);
    let w = 4096;
    let mut worst = 0.0f32;
    let mut pos = 24_000;
    while pos + w + LATENCY < n {
        let seg = &out[pos + LATENCY..pos + LATENCY + w];
        let spec = magnitude_spectrum_db(seg);
        let bin = SR / w as f32;
        let (pk, _) = spec.iter().enumerate().skip(5).fold((0, f32::MIN), |b, (i, &v)| if v > b.1 { (i, v) } else { b });
        let (a, m, z) = (spec[pk - 1], spec[pk], spec[pk + 1]);
        let fp = (pk as f32 + 0.5 * (a - z) / (a - 2.0 * m + z)) * bin;
        let f0 = f_at((pos + w / 2) as f64 / SR as f64) as f32;
        let k = (fp / f0).round().max(1.0);
        let cents = 1200.0 * (fp / (k * f0)).log2();
        worst = worst.max(cents.abs());
        pos += 4800;
    }
    println!("glide: worst {worst:.2} cents off the input's harmonic");
    assert!(worst < 5.0, "output pitch drifted {worst:.2} cents");
}

#[test]
fn unvoiced_noise_passes_without_tones() {
    let n = (4.0 * SR) as usize;
    let mut s = 0x9E37_79B9u32;
    let noise: Vec<f32> = (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            0.3 * (s as f32 / u32::MAX as f32 - 0.5)
        })
        .collect();
    let (out, e) = run(&noise, &[], &settings(1.0, Source::Model), kubyz_model(1), None);
    assert!(!e.frame.voiced, "white noise must not be called voiced");
    // Welch spectrum: 4096-point segments averaged in power. A tone is a few
    // bins wide; the median of ±8 bins (±94 Hz) is its local floor. The
    // Kubyz mouth formants (bandwidth ≥ 200 Hz) are the target's timbre and
    // are wider than that window, so they do not count.
    let w = 4096;
    let mut acc = vec![0.0f64; w / 2 + 1];
    let mut segs = 0;
    let mut pos = 24_000;
    while pos + w <= n {
        for (a, d) in acc.iter_mut().zip(magnitude_spectrum_db(&out[pos..pos + w])) {
            *a += 10f64.powf(d as f64 / 10.0);
        }
        segs += 1;
        pos += w / 2;
    }
    let db: Vec<f32> = acc.iter().map(|p| (10.0 * (p / segs as f64).log10()) as f32).collect();
    let bin = SR / w as f32;
    let mut worst = (0.0f32, 0.0f32);
    for i in (200.0 / bin) as usize..(15_000.0 / bin) as usize {
        let mut win: Vec<f32> = db[i.saturating_sub(8)..(i + 8).min(db.len())].to_vec();
        win.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let excess = db[i] - win[win.len() / 2];
        if excess > worst.0 {
            worst = (excess, i as f32 * bin);
        }
    }
    println!("noise: worst peak {:.1} dB over its local median at {:.0} Hz", worst.0, worst.1);
    assert!(worst.0 < 6.0, "tonal artefact {:.1} dB at {:.0} Hz", worst.0, worst.1);
    let rms = |x: &[f32]| (x[24_000..].iter().map(|v| v * v).sum::<f32>() / (x.len() - 24_000) as f32).sqrt();
    let dl = 20.0 * (rms(&out) / rms(&noise)).log10();
    assert!(dl.abs() < 3.0, "noise level moved {dl:.1} dB");
}


#[test]
fn envelope_mode_moves_noise_to_the_target_colour() {
    // Main: white noise. Target: white noise through a 1 kHz low-pass (−12
    // dB/oct above). Envelope mode at Morph 1 must give the main noise the
    // target's slope; the measured drop 1 → 4 kHz is compared.
    use superduper_synth_core::dsp_blocks::Biquad;
    let n = (4.0 * SR) as usize;
    let mut st = [0x9E37_79B9u32, 0x1357_9BDFu32];
    let mut white = |j: usize| {
        let s = &mut st[j];
        *s ^= *s << 13;
        *s ^= *s >> 17;
        *s ^= *s << 5;
        0.3 * (*s as f32 / u32::MAX as f32 - 0.5)
    };
    let main: Vec<f32> = (0..n).map(|_| white(0)).collect();
    let mut lp = Biquad::default();
    lp.set_lpf(SR, 1000.0, 0.707);
    let sc: Vec<f32> = (0..n).map(|_| lp.process(white(1)) * 4.0).collect();
    let s = Settings { morph: 1.0, source: Source::Sidechain, harmonic: false, ..Settings::default() };
    let (out, _) = run(&main, &sc, &s, kubyz_model(1), None);
    let welch = |x: &[f32], f: f32| {
        let w = 4096;
        let b = (f / (SR / w as f32)) as usize;
        let mut acc = 0.0f64;
        let mut pos = 24_000;
        let mut segs = 0;
        while pos + w <= x.len() {
            let spec = magnitude_spectrum_db(&x[pos..pos + w]);
            acc += spec[b - 20..b + 20].iter().map(|d| 10f64.powf(*d as f64 / 10.0)).sum::<f64>();
            segs += 1;
            pos += w;
        }
        (10.0 * (acc / segs as f64).log10()) as f32
    };
    let slope = |x: &[f32]| welch(x, 4000.0) - welch(x, 1000.0);
    let (s_in, s_sc, s_out) = (slope(&main), slope(&sc), slope(&out));
    println!("1→4 kHz: main {s_in:.1} dB, target {s_sc:.1} dB, output {s_out:.1} dB");
    assert!((s_out - s_sc).abs() < 3.0, "output slope {s_out:.1} vs target {s_sc:.1}");
}

/// `cargo test --release -p superduper-morph --test dsp_smoke cpu_cost -- --ignored --nocapture`
#[test]
#[ignore]
fn cpu_cost() {
    let main = tone(F0, &voice_db(), 10.0);
    let sc = tone(F0_SC, &sc_profile_db(), 10.0);
    let s = settings(1.0, Source::Sidechain);
    let t = std::time::Instant::now();
    let (out, _) = run(&main, &sc, &s, kubyz_model(1), None);
    let el = t.elapsed().as_secs_f64();
    println!("10 s of stereo audio in {:.3} s → {:.2} % of one core (checksum {})", el, el * 10.0, out[out.len() / 2]);
}
