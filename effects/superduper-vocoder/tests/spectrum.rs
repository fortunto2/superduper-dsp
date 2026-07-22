//! Spectrum-based tests for SuperDuper Vocoder. Renders ASCII spectra so a
//! human (or AI) can SEE the comb-like band structure without opening REAPER.
//!
//! Run: `cargo test -p superduper-vocoder --test spectrum -- --nocapture`

use superduper_synth_core::analysis::{ascii_spectrum, spectrum_with_freq, AsciiSpectrumOpts};
use superduper_vocoder::dsp::{
    band_center_hz, VocParams, Vocoder, MAX_VOICES, MODE_CLASSIC, PITCH_AUTO, SRC_INTERNAL,
    WAVE_SAW,
};

const SR: f32 = 48_000.0;
const BANDS: usize = 16;

fn params() -> VocParams {
    VocParams {
        attack_ms: 5.0,
        release_ms: 40.0,
        source: SRC_INTERNAL,
        wave: WAVE_SAW,
        band_count: BANDS,
        pitch_source: PITCH_AUTO,
        notes: [-1; MAX_VOICES],
        pitch_offset_semi: 0.0,
        detune_cents: 0.0,
        formant_semi: 0.0,
        unvoiced: 0.0,
        drive: 0.0,
        mix: 1.0,
        output_lin: 1.0,
        mode: MODE_CLASSIC,
        detail: 1,
        bypassed: false,
    }
}

fn white_noise(n: usize) -> Vec<f32> {
    let mut rng = 0x1234_5678u32;
    (0..n)
        .map(|_| {
            rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((rng >> 16) as f32 / 32768.0 - 1.0) * 0.35
        })
        .collect()
}

#[test]
fn print_band_centres() {
    for &count in &[11usize, 16, 20] {
        println!("\nVocoder band centres (mel-spaced, {count} bands):");
        for i in 0..count {
            println!("  band {:2}: {:>8.1} Hz", i, band_center_hz(i, count));
        }
        // Monotonically increasing and inside the audible range.
        for i in 1..count {
            assert!(band_center_hz(i, count) > band_center_hz(i - 1, count));
        }
        assert!(band_center_hz(0, count) > 60.0 && band_center_hz(count - 1, count) < 9000.0);
    }
}

#[test]
fn broadband_modulator_shows_carrier_comb() {
    // White-noise modulator opens every band, so the output is the internal
    // saw carrier reconstructed through all 16 bands — a harmonic comb of the
    // tracked pitch (silent/noisy input holds the 110 Hz default).
    let n = (SR * 2.0) as usize;
    let modulator = white_noise(n);
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let sc = vec![0.0f32; n];

    let mut voc = Vocoder::new(SR);
    voc.process_stereo(&modulator, &modulator, &mut out_l, &mut out_r, &sc, &sc, &params());

    let fft_len = 16384;
    let start = n / 2;
    let window: Vec<f32> = (0..fft_len).map(|i| out_l[start + i]).collect();
    let spec = spectrum_with_freq(&window, SR);

    println!("\nVocoder output — broadband modulator + internal saw carrier:");
    println!("{}", ascii_spectrum(&spec, &AsciiSpectrumOpts::default()));

    let audible = spec.iter().any(|&(f, db)| f > 100.0 && f < 8000.0 && db > -60.0);
    assert!(audible, "output spectrum entirely below -60 dB — vocoder produced nothing");
}

#[test]
fn tonal_modulator_selects_bands() {
    // A modulator concentrated near ~1 kHz should push energy toward the
    // bands around 1 kHz — the vocoder tracking the modulator's spectral peak.
    use std::f32::consts::TAU;
    let n = (SR * 2.0) as usize;
    let modulator: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            // Narrow-ish energy near 1 kHz (fundamental 1 kHz + a little 2 kHz).
            0.3 * (TAU * 1000.0 * t).sin() + 0.1 * (TAU * 2000.0 * t).sin()
        })
        .collect();
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let sc = vec![0.0f32; n];

    let mut p = params();
    p.release_ms = 60.0;
    let mut voc = Vocoder::new(SR);
    voc.process_stereo(&modulator, &modulator, &mut out_l, &mut out_r, &sc, &sc, &p);

    let fft_len = 16384;
    let start = n / 2;
    let window: Vec<f32> = (0..fft_len).map(|i| out_l[start + i]).collect();
    let spec = spectrum_with_freq(&window, SR);

    println!("\nVocoder output — 1 kHz-ish tonal modulator (energy should cluster mid-band):");
    println!("{}", ascii_spectrum(&spec, &AsciiSpectrumOpts::default()));

    fn band_avg(spec: &[(f32, f32)], lo: f32, hi: f32) -> f32 {
        let v: Vec<f32> = spec.iter().filter(|(f, _)| *f >= lo && *f <= hi).map(|&(_, d)| d).collect();
        if v.is_empty() { -120.0 } else { v.iter().sum::<f32>() / v.len() as f32 }
    }
    let mid = band_avg(&spec, 700.0, 2500.0);
    let high = band_avg(&spec, 5000.0, 9000.0);
    println!("mid (0.7-2.5 kHz) = {mid:.1} dB   high (5-9 kHz) = {high:.1} dB");
    // The mid region (where the modulator lives) must carry more energy than
    // the far-high bands that the modulator barely excites.
    assert!(mid > high, "expected mid-band energy > high-band (mid={mid:.1}, high={high:.1})");
}
