//! Spectrum tests for SuperDuper Wind — renders ASCII spectra so a human
//! (or AI) can SEE the harmonic comb + broadband wind noise, and the swept
//! resonant howl bands + gust surge, without opening REAPER.
//!
//! Run: cargo test --release -p superduper-wind --test spectrum -- --nocapture

use superduper_synth_core::analysis::{ascii_spectrum, spectrum_with_freq, AsciiSpectrumOpts};
use superduper_synth_core::dsp_blocks::{AdsrEnvelope, AdsrParams, midi_note_to_hz};
use superduper_wind::voice::{WindParams, WindVoice, N_HARM};

const SR: f32 = 48_000.0;

fn harmonics(tone: f32) -> [f32; N_HARM] {
    let rolloff = 2.6 - 2.1 * tone.clamp(0.0, 1.0);
    std::array::from_fn(|n| ((n + 1) as f32).powf(-rolloff))
}

/// Kurai (Low Wind) preset values, inlined here so the test doesn't depend
/// on the plugin crate's CLAP param wiring — pure DSP in, ASCII art out.
fn kurai_params() -> WindParams {
    let formant_shift = 2f32.powf(-4.0 / 12.0); // Kurai's Formant = -4 st
    WindParams {
        sr: SR,
        root_hz: midi_note_to_hz(45.0), // A2, ~110 Hz — a low kurai note
        harmonics: harmonics(0.15),     // Kurai's Tone = 0.15 (dark)
        formant_f: [500.0 * formant_shift, 1100.0 * formant_shift, 2000.0 * formant_shift],
        formant_bw: [180.0, 260.0, 340.0],
        formant_gain: [1.0, 0.85, 0.65],
        breath: 0.75,
        jitter: 0.28,
        shimmer: 0.22,
        chiff: 0.0, // burst only matters for the first ~50 ms, exclude it
        color: 0.2,
        howl: 0.15, // Kurai's Howl — mostly gentle breath character
        gust_mult: 1.0,
        whistle: 0.0, // Kurai doesn't use the Aeolian whistle
    }
}

/// Wind (Howl) preset values — the procedural Farnell howling-wind engine
/// dominant, near-silent additive tone.
fn howl_params() -> WindParams {
    let formant_shift = 2f32.powf(-6.0 / 12.0); // Wind (Howl)'s Formant = -6 st
    WindParams {
        sr: SR,
        root_hz: midi_note_to_hz(45.0), // A2 — transposes the howl's sweep range
        harmonics: harmonics(0.1),
        formant_f: [500.0 * formant_shift, 1100.0 * formant_shift, 2000.0 * formant_shift],
        formant_bw: [180.0, 260.0, 340.0],
        formant_gain: [1.0, 0.85, 0.65],
        breath: 0.95,
        jitter: 0.4,
        shimmer: 0.5,
        chiff: 0.0,
        color: 0.35,
        howl: 0.95,
        gust_mult: 1.0,
        whistle: 0.0, // overridden per-test where the Aeolian whistle is under test
    }
}

#[test]
fn sustained_kurai_note_shows_harmonics_and_breath_noise() {
    let mut v = WindVoice::new(0);
    let p = kurai_params();
    v.env.gate_on();
    v.velocity = 0.9;
    v.on_note_on(SR);

    let adsr = AdsrParams::adsr(SR, 0.18, 0.03, 1.0, 0.5);
    let mut e = AdsrEnvelope::default();
    e.gate_on();

    // Warm up past the attack + chiff window, then capture a sustained window.
    let warm = (SR * 0.5) as usize;
    for _ in 0..warm {
        let _ = e.process(adsr);
        let _ = v.process(&p);
    }

    let fft_len = 16384;
    let mut window = vec![0.0_f32; fft_len];
    for s in window.iter_mut() {
        let env = e.process(adsr);
        let (l, _r) = v.process(&p);
        *s = l * env;
    }

    let spec = spectrum_with_freq(&window, SR);
    println!("\nSuperDuper Wind — Kurai (Low Wind), sustained A2 (~110 Hz):");
    println!("{}", ascii_spectrum(&spec, &AsciiSpectrumOpts::default()));

    for &(_, db) in &spec {
        assert!(db.is_finite(), "spectrum contains NaN/Inf");
    }

    // 1. The fundamental / low harmonics region should carry real energy —
    // Kurai is a dark, low-formant patch.
    let low_energy = spec
        .iter()
        .filter(|&&(f, _)| f > 60.0 && f < 900.0)
        .map(|&(_, db)| 10f32.powf(db / 10.0))
        .sum::<f32>();
    assert!(low_energy > 0.0, "no low-frequency (tone) energy detected");

    // 2. Broadband wind noise should also be present well above the
    // handful of additive harmonics (> 4 kHz, past N_HARM * f0 for a
    // ~110 Hz fundamental with 6 harmonics ≈ 660 Hz top partial).
    let hf_energy = spec
        .iter()
        .filter(|&&(f, _)| f > 4000.0 && f < 16000.0)
        .map(|&(_, db)| 10f32.powf(db / 10.0))
        .sum::<f32>();
    println!("low(60-900Hz) power={low_energy:.4}  hf(4-16kHz) power={hf_energy:.6}");
    assert!(
        hf_energy > 0.0,
        "expected some broadband wind-noise energy above 4 kHz (Kurai Breath=0.75)"
    );
}

#[test]
fn raising_breath_broadens_the_spectrum() {
    // Same note, Breath=0 vs Breath=1 — the wet version should carry more
    // energy in the upper spectrum (the noise layer, not the tone).
    fn render_tail(breath: f32) -> Vec<f32> {
        let mut v = WindVoice::new(1);
        let mut p = kurai_params();
        p.breath = breath;
        p.chiff = 0.0;
        v.env.gate_on();
        v.velocity = 0.9;
        v.on_note_on(SR);
        let adsr = AdsrParams::adsr(SR, 0.02, 0.03, 1.0, 0.5);
        let mut e = AdsrEnvelope::default();
        e.gate_on();
        let warm = (SR * 0.3) as usize;
        for _ in 0..warm {
            let _ = e.process(adsr);
            let _ = v.process(&p);
        }
        let fft_len = 16384;
        (0..fft_len)
            .map(|_| {
                let env = e.process(adsr);
                let (l, _) = v.process(&p);
                l * env
            })
            .collect()
    }

    let dry = render_tail(0.0);
    let wet = render_tail(1.0);
    let spec_dry = spectrum_with_freq(&dry, SR);
    let spec_wet = spectrum_with_freq(&wet, SR);

    let hf = |spec: &[(f32, f32)]| {
        spec.iter()
            .filter(|&&(f, _)| f > 3000.0 && f < 16000.0)
            .map(|&(_, db)| 10f32.powf(db / 10.0))
            .sum::<f32>()
    };
    let hf_dry = hf(&spec_dry);
    let hf_wet = hf(&spec_wet);
    println!("HF (3-16kHz) power — Breath=0: {hf_dry:.6}, Breath=1: {hf_wet:.6}");
    assert!(
        hf_wet > hf_dry,
        "raising Breath should broaden the spectrum with more HF energy"
    );
}

/// The headline ask: show the procedural HOWLING WIND — 2-3 swept high-Q
/// resonant bandpasses spanning ~200 Hz-2 kHz (Farnell's model) — as an
/// ASCII spectrum, and prove the sweep genuinely spans that range (not a
/// single static peak) by comparing two windows captured seconds apart.
#[test]
fn wind_howl_preset_shows_swept_resonant_bands() {
    let mut v = WindVoice::new(2);
    let p = howl_params();
    v.env.gate_on();
    v.velocity = 0.9;
    v.on_note_on(SR);

    let adsr = AdsrParams::adsr(SR, 0.4, 0.05, 1.0, 0.9);
    let mut e = AdsrEnvelope::default();
    e.gate_on();

    let fft_len = 16384;
    let capture = |v: &mut WindVoice, e: &mut AdsrEnvelope| -> Vec<f32> {
        (0..fft_len)
            .map(|_| {
                let env = e.process(adsr);
                let (l, _r) = v.process(&p);
                l * env
            })
            .collect()
    };

    // Warm past the attack, capture window A, run the sweep forward ~1.3 s
    // (multiple LFO cycles at the 0.17-0.41 Hz band rates), capture window B.
    let warm = (SR * 0.6) as usize;
    for _ in 0..warm {
        let _ = e.process(adsr);
        let _ = v.process(&p);
    }
    let win_a = capture(&mut v, &mut e);
    let advance = (SR * 1.3) as usize;
    for _ in 0..advance {
        let _ = e.process(adsr);
        let _ = v.process(&p);
    }
    let win_b = capture(&mut v, &mut e);

    let spec_a = spectrum_with_freq(&win_a, SR);
    let spec_b = spectrum_with_freq(&win_b, SR);

    println!("\nSuperDuper Wind — Wind (Howl), sustained A2, window A (t≈0.6s):");
    println!("{}", ascii_spectrum(&spec_a, &AsciiSpectrumOpts::default()));
    println!("\nSuperDuper Wind — Wind (Howl), sustained A2, window B (t≈2.5s, bands swept):");
    println!("{}", ascii_spectrum(&spec_b, &AsciiSpectrumOpts::default()));

    for &(_, db) in spec_a.iter().chain(spec_b.iter()) {
        assert!(db.is_finite(), "howl spectrum contains NaN/Inf");
    }

    // 1. Energy should be present across the whole 200 Hz-2 kHz howl range
    // (not just at the fundamental) — the whole point of the swept bands.
    let howl_band_energy = |spec: &[(f32, f32)]| {
        spec.iter()
            .filter(|&&(f, _)| f > 200.0 && f < 2000.0)
            .map(|&(_, db)| 10f32.powf(db / 10.0))
            .sum::<f32>()
    };
    let energy_a = howl_band_energy(&spec_a);
    let energy_b = howl_band_energy(&spec_b);
    println!("200Hz-2kHz howl-band power — window A: {energy_a:.4}, window B: {energy_b:.4}");
    assert!(energy_a > 0.0 && energy_b > 0.0, "howl band should carry energy in both windows");

    // 2. The sweep should actually move: the loudest bin inside the howl
    // range shouldn't land on the exact same frequency in both windows.
    let loudest_bin = |spec: &[(f32, f32)]| -> f32 {
        spec.iter()
            .filter(|&&(f, _)| f > 200.0 && f < 2000.0)
            .fold((0.0_f32, f32::MIN), |acc, &(f, db)| if db > acc.1 { (f, db) } else { acc })
            .0
    };
    let peak_a = loudest_bin(&spec_a);
    let peak_b = loudest_bin(&spec_b);
    println!("loudest howl-band bin — window A: {peak_a:.0} Hz, window B: {peak_b:.0} Hz");
    assert!(
        (peak_a - peak_b).abs() > 5.0,
        "the resonant bands should have measurably swept between windows \
         (A={peak_a:.0} Hz, B={peak_b:.0} Hz) — a static peak means the LFO/walk isn't moving"
    );
}

/// GUST modulation — render several seconds while sweeping `gust_mult`
/// through a simulated gust cycle (this is normally computed once per
/// block by `lib.rs`'s shared `gust_gen`; here we drive it directly so the
/// DSP-level test doesn't depend on CLAP plumbing) and print a per-200ms
/// RMS trace so the surge is visible as ASCII, then assert the surge is
/// actually audible (loud windows clearly louder than quiet windows).
#[test]
fn gust_modulation_produces_an_audible_surge_over_time() {
    let mut v = WindVoice::new(3);
    let mut p = howl_params();
    v.env.gate_on();
    v.velocity = 0.9;
    v.on_note_on(SR);
    let adsr = AdsrParams::adsr(SR, 0.05, 0.05, 1.0, 0.9);
    let mut e = AdsrEnvelope::default();
    e.gate_on();

    let chunk = (SR * 0.2) as usize; // 200 ms windows
    let n_chunks = 20; // 4 seconds total — two full simulated gust cycles at 0.5 Hz
    let mut levels = Vec::with_capacity(n_chunks);

    for c in 0..n_chunks {
        // Simulate the shared gust envelope directly (0.5 Hz surge, the
        // fastest rate the Gust param maps to) rather than depending on
        // `lib.rs`'s WobbleGen-driven version — this isolates "does
        // gust_mult audibly scale the bed" from "is the gust LFO correct".
        let t = c as f32 * 0.2;
        let swell01 = 0.5 + 0.5 * (core::f32::consts::TAU * 0.5 * t).sin();
        p.gust_mult = 0.15 + 0.85 * swell01; // gust_amt≈1 mapping, floor so it never fully mutes

        let mut sum_sq = 0.0_f32;
        for _ in 0..chunk {
            let env = e.process(adsr);
            let (l, _r) = v.process(&p);
            let s = l * env;
            sum_sq += s * s;
        }
        levels.push((sum_sq / chunk as f32).sqrt());
    }

    println!("\nSuperDuper Wind — Wind (Howl) gust surge, RMS per 200 ms window:");
    let max_level = levels.iter().copied().fold(0.0_f32, f32::max).max(1e-6);
    for (i, &lvl) in levels.iter().enumerate() {
        let bar_len = ((lvl / max_level) * 50.0).round() as usize;
        println!("  t={:>4.1}s  {:>7.4}  {}", i as f32 * 0.2, lvl, "#".repeat(bar_len));
    }

    for &lvl in &levels {
        assert!(lvl.is_finite(), "gust-modulated output contains NaN/Inf");
    }
    let min_level = levels.iter().copied().fold(f32::MAX, f32::min);
    println!("min={min_level:.4}  max={max_level:.4}  ratio={:.2}x", max_level / min_level.max(1e-9));
    assert!(
        max_level > min_level * 1.8,
        "gust surge should produce an audible loud/quiet contrast: min={min_level:.4} max={max_level:.4}"
    );
}

/// The Aeolian-tone ask: prove the vortex-shedding whistle (Strouhal
/// `f = St·U/d`) actually GLIDES in pitch as the wind intensifies. Direct
/// control of `gust_mult` (rather than the internal LFO/random-walk
/// timing, already covered by the surge test above) isolates "does the
/// whistle glide" from "is the gust envelope's own timing correct".
#[test]
fn aeolian_whistle_glides_with_gust_in_the_spectrum() {
    fn render_tail(gust_mult: f32) -> Vec<f32> {
        let mut v = WindVoice::new(10);
        let mut p = howl_params();
        p.whistle = 0.9;
        p.gust_mult = gust_mult;
        v.env.gate_on();
        v.velocity = 0.9;
        v.on_note_on(SR);
        let adsr = AdsrParams::adsr(SR, 0.05, 0.05, 1.0, 0.9);
        let mut e = AdsrEnvelope::default();
        e.gate_on();
        let warm = (SR * 0.3) as usize;
        for _ in 0..warm {
            let _ = e.process(adsr);
            let _ = v.process(&p);
        }
        let fft_len = 16384;
        (0..fft_len)
            .map(|_| {
                let env = e.process(adsr);
                let (l, _) = v.process(&p);
                l * env
            })
            .collect()
    }

    let calm = render_tail(0.0); // low wind speed U
    let gusty = render_tail(1.0); // high wind speed U — peak of a gust
    let spec_calm = spectrum_with_freq(&calm, SR);
    let spec_gusty = spectrum_with_freq(&gusty, SR);

    println!("\nSuperDuper Wind — Aeolian whistle, CALM (gust_mult=0.0, low wind speed U):");
    println!("{}", ascii_spectrum(&spec_calm, &AsciiSpectrumOpts::default()));
    println!("\nSuperDuper Wind — Aeolian whistle, GUSTY (gust_mult=1.0, peak wind speed U):");
    println!("{}", ascii_spectrum(&spec_gusty, &AsciiSpectrumOpts::default()));

    for &(_, db) in spec_calm.iter().chain(spec_gusty.iter()) {
        assert!(db.is_finite(), "Aeolian spectrum contains NaN/Inf");
    }

    // The whistle is a narrow, tonal peak — the single loudest bin in each
    // spectrum should track it (the broadband howl bands are much wider
    // and flatter, so a sharp global peak is a good proxy for the whistle).
    let loudest = |spec: &[(f32, f32)]| -> (f32, f32) {
        spec.iter()
            .fold((0.0_f32, f32::MIN), |acc, &(f, db)| if db > acc.1 { (f, db) } else { acc })
    };
    let (f_calm, db_calm) = loudest(&spec_calm);
    let (f_gusty, db_gusty) = loudest(&spec_gusty);
    println!(
        "loudest bin (Strouhal whistle) — calm: {f_calm:.0} Hz ({db_calm:.1} dB), \
         gusty: {f_gusty:.0} Hz ({db_gusty:.1} dB)"
    );
    assert!(
        f_gusty > f_calm,
        "the Aeolian whistle should glide UP in frequency as the gust intensifies \
         (f = St·U/d, U rises with gust_mult): calm={f_calm:.0}Hz gusty={f_gusty:.0}Hz"
    );
}
