//! Tests for `timbre.rs`, in their own file so the module stays under the
//! 1000-line limit; `#[path]` keeps them `mod tests` of it.

use super::*;

const N: usize = 2048;

/// `secs` of harmonics of `f0` with linear amplitudes `amps` (h1 first).
fn harmonic_tone(sr: f32, f0: f32, amps: &[f32], secs: f32) -> Vec<f32> {
    (0..(sr * secs) as usize)
        .map(|i| {
            let t = i as f64 / sr as f64;
            amps.iter()
                .enumerate()
                .map(|(k, &a)| a as f64 * (core::f64::consts::TAU * f0 as f64 * (k + 1) as f64 * t).sin())
                .sum::<f64>() as f32
        })
        .collect()
}

fn white_noise(n: usize, mut s: u32, amp: f32) -> Vec<f32> {
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            amp * (s as f32 / u32::MAX as f32 - 0.5)
        })
        .collect()
}

/// Hann-windowed spectrum of one `N`-sample frame (what the STFT hands over).
fn hann_spectrum(frame: &[f32]) -> Vec<Complex<f32>> {
    use realfft::RealFftPlanner;
    let n = frame.len();
    let mut x: Vec<f32> = frame
        .iter()
        .enumerate()
        .map(|(i, &v)| v * (0.5 - 0.5 * (core::f32::consts::TAU * i as f32 / n as f32).cos()))
        .collect();
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
    let mut out = fft.make_output_vec();
    fft.process(&mut x, &mut out).unwrap();
    out
}

/// The engine's view of `sig`: every sample through the ring, the last
/// `N` through the Hann STFT, then one `analyze` with `hint`.
fn analyzer_for(sr: f32, sig: &[f32], hint: f32) -> (Analyzer, FrameAnalysis) {
    let mut an = Analyzer::new(N / 2, sr / N as f32);
    sig.iter().for_each(|&x| an.push(x));
    let mut fa = FrameAnalysis::default();
    an.analyze(&hann_spectrum(&sig[sig.len() - N..]), hint, &mut fa);
    (an, fa)
}

fn analyze_signal(sr: f32, sig: &[f32], hint: f32) -> FrameAnalysis {
    analyzer_for(sr, sig, hint).1
}

/// 30 harmonics falling 1/k — a bright, clean pitched tone.
fn bright_amps() -> Vec<f32> {
    (1..=30).map(|k| 0.3 / k as f32).collect()
}

/// The jaw-harp shape: 12 harmonics of 130 Hz, 5–9 at 0 dB, the rest −20 dB.
fn drone(sr: f32, secs: f32) -> Vec<f32> {
    let amps: Vec<f32> = (1..=12).map(|k| if (5..=9).contains(&k) { 0.1 } else { 0.01 }).collect();
    harmonic_tone(sr, 130.0, &amps, secs)
}

// Before the time-domain measure (spectral lobe ratio on the 2048 frame),
// measured here: 48 kHz 60 Hz → 0.19, 80 Hz → 0.45 (unvoiced); 96 kHz
// 120 Hz → 0.19, 150 Hz → 0.00; white noise with a 60–80 Hz hint at
// 96 kHz → 1.00 "voiced"; the drone at 96 kHz → 0.00 even at the right hint.
#[test]
fn low_tones_are_voiced_at_any_sample_rate() {
    for &sr in &[48_000.0f32, 96_000.0] {
        for &f0 in &[50.0f32, 60.0, 80.0, 100.0, 120.0, 150.0] {
            let fa = analyze_signal(sr, &harmonic_tone(sr, f0, &bright_amps(), 0.2), f0);
            assert!(fa.harmonicity >= 0.8 && fa.voiced, "{sr} Hz, {f0} Hz: harmonicity {:.2}", fa.harmonicity);
            assert!((fa.f0 / f0 - 1.0).abs() < 0.01, "{sr} Hz, {f0} Hz: f0 read {:.2}", fa.f0);
        }
    }
}

#[test]
fn white_noise_is_unvoiced_whatever_the_hint() {
    for &sr in &[48_000.0f32, 96_000.0] {
        let noise = white_noise(sr as usize / 5, 0x9E37_79B9, 0.6);
        for &hint in &[60.0f32, 80.0, 100.0, 200.0, 500.0] {
            let fa = analyze_signal(sr, &noise, hint);
            assert!(fa.harmonicity <= 0.3 && !fa.voiced, "{sr} Hz, hint {hint}: harmonicity {:.2}", fa.harmonicity);
        }
    }
}

#[test]
fn a_formant_heavy_drone_is_read_at_its_fundamental() {
    for &sr in &[48_000.0f32, 96_000.0] {
        let d = drone(sr, 0.5);
        // The hint the live jaw harp produced (its 5–9 harmonic group), and
        // the one the engine's own tracker gives.
        let mut yin = crate::pitch::YinPitchTracker::new(sr, 55.0, 1000.0, 2048, 256, 150.0);
        d.iter().for_each(|&x| {
            yin.push(x);
        });
        for hint in [943.0, yin.current_hz()] {
            let fa = analyze_signal(sr, &d, hint);
            assert!((fa.f0 / 130.0 - 1.0).abs() < 0.03, "{sr} Hz, hint {hint:.1}: f0 {:.1}", fa.f0);
            assert!(fa.voiced, "{sr} Hz, hint {hint:.1}: harmonicity {:.2}", fa.harmonicity);
        }
        // The octave is not given away: the hint's own period wins a tie.
        let fa = analyze_signal(sr, &d, 130.0);
        assert!((fa.f0 - 130.0).abs() < 1.0, "f0 {:.1}", fa.f0);
    }
}

#[test]
fn a_stale_hint_lands_on_the_real_period_not_a_subharmonic() {
    // The tracker holds its last pitch through a consonant and starts at
    // 150 Hz: on a 220 Hz note 2 × (1/150 s) ≈ 3 × (1/220 s), and the
    // multiples search alone called that voiced at 73 Hz (measured on
    // the quality snapshot's voice: +5–6 dB in the sub/kick/low bands
    // from "harmonics" at 73 and 146 Hz raised toward the model's).
    for &(hint, f0) in &[(150.0f32, 220.0f32), (220.0, 330.0), (220.0, 300.0), (330.0, 220.0)] {
        let fa = analyze_signal(48_000.0, &harmonic_tone(48_000.0, f0, &bright_amps(), 0.2), hint);
        assert!((fa.f0 / f0 - 1.0).abs() < 0.01, "hint {hint} on {f0} Hz: f0 read {:.1}", fa.f0);
        assert!(fa.voiced, "hint {hint} on {f0} Hz: harmonicity {:.2}", fa.harmonicity);
    }
}

#[test]
fn measured_profile_rolls_off_above_its_last_harmonic() {
    // A 1 kHz target measures 16 harmonics (16 kHz ceiling); a 220 Hz
    // voice has 40. Harmonics 17–40 must continue from h16 at −18 dB/oct,
    // not sit at the −120 dB floor (= a −60 dB gain-limited low-pass).
    let mut p = TimbreProfile { f0: 1000.0, harm_valid: true, env_valid: true, ..Default::default() };
    for k in 0..16 {
        p.harm_db[k] = -(k as f32);
    }
    p.env_db = [-30.0; GRID];
    let mut t = TargetFrame::default();
    t.from_profile(&p, true);
    for k in 16..MAX_HARM {
        let want = t.harm_db[15] - 18.0 * ((k + 1) as f32 / 16.0).log2();
        assert!((t.harm_db[k] - want).abs() < 0.01, "h{}: {:.1} vs {:.1}", k + 1, t.harm_db[k], want);
    }
    let amps: Vec<f32> = (1..=40).map(|k| 0.1 / k as f32).collect();
    let mut sc = GainScratch::new();
    let mut g = vec![0.0; N / 2 + 1];
    let s = MorphSettings { morph: 1.0, vowels: 0.0, breath: 0.3, tilt_db_oct: 0.0, harmonic: true };
    let fpb = 48_000.0 / N as f32;
    let (an, fa) = analyzer_for(48_000.0, &harmonic_tone(48_000.0, 220.0, &amps, 0.2), 220.0);
    assert_eq!(fa.n_harm, 40);
    assert_eq!(morph_gains(&fa, an.power(), fpb, &t, &s, &mut sc, &mut g), FrameMode::Harmonic);
    for k in 16..MAX_HARM {
        let slope = sc.show_target[k] - sc.show_target[15];
        let want = -18.0 * ((k + 1) as f32 / 16.0).log2();
        assert!((slope - want).abs() < 0.01, "h{} target {:.1} dB re h16, want {:.1}", k + 1, slope, want);
    }
}

#[test]
fn breath_scales_only_the_noise_inside_the_comb() {
    // A 100 Hz voice: the comb is tracked to h40 = 4 kHz. Breath must move
    // the bins between harmonics and leave the spectrum above the comb
    // alone (it took the full Breath cut before — a shelf at 4 kHz).
    let sr = 48_000.0;
    let fpb = sr / N as f32;
    let amps: Vec<f32> = (1..=40).map(|k| 0.1 / k as f32).collect();
    let (an, fa) = analyzer_for(sr, &harmonic_tone(sr, 100.0, &amps, 0.2), 100.0);
    assert!(fa.voiced && fa.n_harm == 40, "voiced {} n_harm {}", fa.voiced, fa.n_harm);
    let m = ModelTarget {
        harmonics: [1.0; N_HARMONICS],
        bands: FormantBands::three([705.0, 1301.0, 2165.0], [200.0, 300.0, 400.0], [1.0, 0.9, 0.75]),
        vox_mix: 1.0,
    };
    let mut t = TargetFrame::default();
    t.from_model(&m, fa.f0);
    let run = |breath: f32| {
        let mut sc = GainScratch::new();
        let mut g = vec![0.0; N / 2 + 1];
        let s = MorphSettings { morph: 1.0, vowels: 0.0, breath, tilt_db_oct: 0.0, harmonic: true };
        assert_eq!(morph_gains(&fa, an.power(), fpb, &t, &s, &mut sc, &mut g), FrameMode::Harmonic);
        g
    };
    let (quiet, full) = (run(0.1), run(1.0));
    let bin = |hz: f32| (hz / fpb).round() as usize;
    // The loudness match shifts every bin by one constant; read it off h10.
    let c = quiet[bin(1000.0)] - full[bin(1000.0)];
    // At a 4.3-bin spacing the midpoint bin is still partly inside a
    // harmonic lobe, so Breath's −20 dB shows there in part (−13 measured).
    let between = quiet[bin(1050.0)] - full[bin(1050.0)] - c;
    assert!(between < -5.0, "between h10 and h11: Breath 0.1 vs 1 = {between:.1} dB");
    for hz in [4500.0, 6000.0, 8000.0, 12_000.0] {
        let d = quiet[bin(hz)] - full[bin(hz)] - c;
        assert!(d.abs() < 0.01, "{hz} Hz, above the comb: Breath moved it {d:.1} dB");
    }
}

#[test]
fn harmonic_levels_and_voicing_of_a_known_tone() {
    let amps: Vec<f32> = (1..=10).map(|k| 0.3 / k as f32).collect();
    let fa = analyze_signal(48_000.0, &harmonic_tone(48_000.0, 220.0, &amps, 0.2), 220.0);
    assert!(fa.voiced, "harmonicity {}", fa.harmonicity);
    for k in 1..10 {
        let want = 20.0 * (amps[k] / amps[0]).log10();
        let got = fa.harm_db[k] - fa.harm_db[0];
        assert!((got - want).abs() < 0.6, "h{} {got} vs {want}", k + 1);
    }
}

#[test]
fn morph_zero_is_unity_and_level_is_kept() {
    let amps: Vec<f32> = (1..=10).map(|k| 0.3 / k as f32).collect();
    let (an, fa) = analyzer_for(48_000.0, &harmonic_tone(48_000.0, 220.0, &amps, 0.2), 220.0);
    let m = ModelTarget {
        harmonics: [1.0; N_HARMONICS],
        bands: FormantBands::three([705.0, 1301.0, 2165.0], [200.0, 300.0, 400.0], [1.0, 0.9, 0.75]),
        vox_mix: 1.0,
    };
    let mut t = TargetFrame::default();
    t.from_model(&m, 220.0);
    let mut sc = GainScratch::new();
    let mut g = vec![0.0; 1025];
    let mut s = MorphSettings { morph: 0.0, vowels: 0.0, breath: 0.3, tilt_db_oct: 0.0, harmonic: true };
    assert_eq!(morph_gains(&fa, an.power(), 23.4375, &t, &s, &mut sc, &mut g), FrameMode::Pass);
    assert!(g.iter().all(|&x| x == 0.0));
    s.morph = 1.0;
    assert_eq!(morph_gains(&fa, an.power(), 23.4375, &t, &s, &mut sc, &mut g), FrameMode::Harmonic);
    let ein: f32 = an.power().iter().skip(1).sum();
    let eout: f32 = an.power().iter().zip(&g).skip(1).map(|(p, d)| p * 10f32.powf(d / 10.0)).sum();
    assert!((10.0 * (eout / ein).log10()).abs() < 0.01);
}
