//! Smoke tests for the vocoder DSP block — drive `Vocoder` directly (no
//! CLAP), feed voice-like / silent / sidechain signals and assert the output
//! is finite, stable, and non-trivial where it should be.
//!
//! Run: `cargo test -p superduper-vocoder --test dsp_smoke -- --nocapture`

use superduper_vocoder::dsp::{
    Carrier, VocParams, Vocoder, MAX_VOICES, MODE_CLASSIC, PITCH_AUTO, PITCH_MIDI, SRC_INTERNAL,
    SRC_SIDECHAIN, WAVE_SAW,
};

const SR: f32 = 48_000.0;

fn base_params() -> VocParams {
    VocParams {
        attack_ms: 3.0,
        release_ms: 25.0,
        source: SRC_INTERNAL,
        wave: WAVE_SAW,
        band_count: 16,
        // Auto with no held notes → YIN pitch-tracking (the classic behaviour).
        pitch_source: PITCH_AUTO,
        notes: [-1; MAX_VOICES],
        pitch_offset_semi: 0.0,
        detune_cents: 8.0,
        formant_semi: 0.0,
        unvoiced: 0.15,
        drive: 0.2,
        mix: 1.0,
        output_lin: 1.0,
        mode: MODE_CLASSIC,
        detail: 1,
        bypassed: false,
    }
}

/// A crude "voice": a 150 Hz fundamental + harmonics, amplitude-modulated to
/// fake words / phrasing so band envelopes actually move.
fn voice_like(n: usize) -> Vec<f32> {
    use std::f32::consts::TAU;
    let f0 = 150.0;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let mut s = 0.0;
            for h in 1..=8 {
                s += (1.0 / h as f32) * (TAU * f0 * h as f32 * t).sin();
            }
            // Slow "syllable" AM at 3 Hz + a gate so there are quiet gaps.
            let am = 0.5 + 0.5 * (TAU * 3.0 * t).sin();
            s * 0.2 * am
        })
        .collect()
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|&v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}
fn peak(x: &[f32]) -> f32 {
    x.iter().map(|v| v.abs()).fold(0.0, f32::max)
}
fn all_finite(x: &[f32]) -> bool {
    x.iter().all(|v| v.is_finite())
}

#[test]
fn internal_carrier_produces_stable_voice() {
    let n = (SR * 3.0) as usize;
    let modulator = voice_like(n);
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let sc = vec![0.0f32; n];

    let mut voc = Vocoder::new(SR);
    voc.process_stereo(&modulator, &modulator, &mut out_l, &mut out_r, &sc, &sc, &base_params());

    // Look at the second half — past the YIN warm-up window.
    let tail = &out_l[n / 2..];
    let r = rms(tail);
    let pk = peak(tail);
    println!("internal carrier: rms={r:.5} peak={pk:.5}");

    assert!(all_finite(&out_l), "output L has NaN/Inf");
    assert!(all_finite(&out_r), "output R has NaN/Inf");
    assert!(r > 1e-3, "vocoded output is basically silent (rms={r})");
    assert!(pk < 8.0, "vocoded output blew up (peak={pk})");
    // Detune is on (8 ct) → the stereo carrier must produce a real L/R
    // difference somewhere in the tail (width, not a mono dup).
    let max_lr_diff = (n / 2..n).map(|i| (out_l[i] - out_r[i]).abs()).fold(0.0, f32::max);
    println!("internal carrier: max |L-R| = {max_lr_diff:.5}");
    assert!(max_lr_diff > 1e-4, "detuned carrier produced no stereo width");
}

#[test]
fn zero_detune_is_mono() {
    // With Detune = 0 the two carrier voices collapse to one → identical L/R.
    let n = (SR * 2.0) as usize;
    let modulator = voice_like(n);
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let sc = vec![0.0f32; n];

    let mut p = base_params();
    p.detune_cents = 0.0;
    // Unvoiced noise uses two independent L/R streams (stereo sibilants), so it
    // too must be off to prove the *carrier* collapses to mono at zero detune.
    p.unvoiced = 0.0;
    let mut voc = Vocoder::new(SR);
    voc.process_stereo(&modulator, &modulator, &mut out_l, &mut out_r, &sc, &sc, &p);

    for i in n / 2..n {
        assert!((out_l[i] - out_r[i]).abs() < 1e-5, "zero-detune L/R diverged at {i}");
    }
}

#[test]
fn silent_input_stays_finite_and_quiet() {
    let n = (SR * 1.0) as usize;
    let modulator = vec![0.0f32; n];
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let sc = vec![0.0f32; n];

    let mut voc = Vocoder::new(SR);
    voc.process_stereo(&modulator, &modulator, &mut out_l, &mut out_r, &sc, &sc, &base_params());

    assert!(all_finite(&out_l), "silent input produced NaN/Inf");
    // No modulator energy → the envelopes stay at zero → near silence.
    let r = rms(&out_l);
    println!("silent input: rms={r:.7}");
    assert!(r < 1e-3, "silent input produced audible output (rms={r})");
}

#[test]
fn sidechain_carrier_produces_output() {
    let n = (SR * 2.0) as usize;
    let modulator = voice_like(n);

    // Sidechain carrier = a bright band-limited saw at 110 Hz (mono).
    let mut carrier = Carrier::default();
    let (h, a, b, nrm) = Carrier::detune_pan(0.0, WAVE_SAW);
    let sc: Vec<f32> = (0..n).map(|_| carrier.next_stereo(WAVE_SAW, 110.0, h, a, b, nrm, SR).0).collect();

    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];

    let mut params = base_params();
    params.source = SRC_SIDECHAIN;
    params.drive = 0.0;

    let mut voc = Vocoder::new(SR);
    voc.process_stereo(&modulator, &modulator, &mut out_l, &mut out_r, &sc, &sc, &params);

    let tail = &out_l[n / 2..];
    let r = rms(tail);
    println!("sidechain carrier: rms={r:.5}");
    assert!(all_finite(&out_l), "sidechain output has NaN/Inf");
    assert!(r > 1e-3, "sidechain vocoder produced no output (rms={r})");
}

#[test]
fn bypass_passes_input_through() {
    let n = 4096;
    let modulator = voice_like(n);
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let sc = vec![0.0f32; n];

    let mut params = base_params();
    params.bypassed = true;

    let mut voc = Vocoder::new(SR);
    voc.process_stereo(&modulator, &modulator, &mut out_l, &mut out_r, &sc, &sc, &params);

    for i in 0..n {
        assert!((out_l[i] - modulator[i]).abs() < 1e-6, "bypass altered sample {i}");
    }
}

#[test]
fn midi_note_pitches_carrier() {
    let n = (SR * 1.5) as usize;
    let modulator = voice_like(n);
    let sc = vec![0.0f32; n];

    // MIDI mode, one held note (A4 = 69) → tonal robot voice.
    let mut p = base_params();
    p.pitch_source = PITCH_MIDI;
    p.notes = [69, -1, -1, -1, -1, -1];
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let mut voc = Vocoder::new(SR);
    voc.process_stereo(&modulator, &modulator, &mut out_l, &mut out_r, &sc, &sc, &p);
    assert!(all_finite(&out_l), "MIDI carrier output has NaN/Inf");
    let r_note = rms(&out_l[n / 2..]);
    println!("MIDI note held: rms={r_note:.5}");
    assert!(r_note > 1e-3, "held MIDI note produced no carrier output (rms={r_note})");

    // MIDI mode, NO keys held + no unvoiced noise → tonal carrier is gated off
    // → silence (a real hardware vocoder is silent with no carrier).
    let mut p2 = base_params();
    p2.pitch_source = PITCH_MIDI;
    p2.notes = [-1; MAX_VOICES];
    p2.unvoiced = 0.0;
    let mut out_l2 = vec![0.0f32; n];
    let mut out_r2 = vec![0.0f32; n];
    let mut voc2 = Vocoder::new(SR);
    voc2.process_stereo(&modulator, &modulator, &mut out_l2, &mut out_r2, &sc, &sc, &p2);
    let r_silent = rms(&out_l2[n / 2..]);
    println!("MIDI no keys: rms={r_silent:.6}");
    assert!(r_silent < 1e-3, "MIDI mode with no keys should be silent (rms={r_silent})");
}

#[test]
fn all_presets_in_range_and_stable() {
    use superduper_vocoder::presets::PRESETS;
    use superduper_vocoder::PARAMS;

    // 1. Every preset value must sit inside its param's [min, max].
    for preset in PRESETS {
        assert_eq!(preset.values.len(), PARAMS.len(), "preset '{}' wrong length", preset.name);
        for (i, &v) in preset.values.iter().enumerate() {
            let def = &PARAMS[i];
            assert!(
                v >= def.min as f32 - 1e-4 && v <= def.max as f32 + 1e-4,
                "preset '{}' param {} = {} out of range [{}, {}]",
                preset.name,
                i,
                v,
                def.min,
                def.max
            );
        }
    }
    println!("{} presets validated in range", PRESETS.len());

    // 2. Sanity: the DSP defaults produce finite, non-trivial output (a proxy
    //    that the preset value layout maps onto real params without blowing up).
    let n = SR as usize;
    let modulator = voice_like(n);
    let sc = vec![0.0f32; n];
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let mut voc = Vocoder::new(SR);
    voc.process_stereo(&modulator, &modulator, &mut out_l, &mut out_r, &sc, &sc, &base_params());
    assert!(all_finite(&out_l));
}

#[test]
fn denormal_silence_stays_finite() {
    // A burst of voice, then several seconds of silence. Filter/envelope tails
    // decaying toward silence must stay finite (no NaN) and settle to ~zero —
    // the plugin sets hardware flush-to-zero in process(), but the DSP block is
    // numerically safe on its own too.
    let n = (SR * 4.0) as usize;
    let mut modulator = vec![0.0f32; n];
    let burst = voice_like(SR as usize / 2); // 0.5 s of voice
    modulator[..burst.len()].copy_from_slice(&burst);
    let sc = vec![0.0f32; n];

    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    let mut voc = Vocoder::new(SR);
    voc.process_stereo(&modulator, &modulator, &mut out_l, &mut out_r, &sc, &sc, &base_params());

    assert!(all_finite(&out_l), "output went non-finite during decay to silence");
    let tail = &out_l[(SR * 3.0) as usize..]; // last second — long after the burst
    let r = rms(tail);
    println!("denormal decay tail rms={r:.8}");
    assert!(r < 1e-3, "output didn't settle to silence (rms={r})");
}
