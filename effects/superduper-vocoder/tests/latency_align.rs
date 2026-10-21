//! The latency the plugin reports is the delay of its dry path, and the
//! STFT's delay is one whole window: with `STFT_LATENCY = N - HOP` the dry
//! sat 512 samples ahead of the wet and the host's PDC was 512 short
//! (2026-10-21). Pinned here through the DSP block.

use superduper_vocoder::dsp::{
    VocParams, Vocoder, MAX_VOICES, MODE_CLASSIC, MODE_SPECTRAL, PITCH_AUTO, SRC_INTERNAL, STFT_LATENCY, WAVE_SAW,
};

const SR: f32 = 48_000.0;

fn params(mode: u32, mix: f32) -> VocParams {
    VocParams {
        attack_ms: 3.0,
        release_ms: 25.0,
        source: SRC_INTERNAL,
        wave: WAVE_SAW,
        band_count: 16,
        pitch_source: PITCH_AUTO,
        notes: [-1; MAX_VOICES],
        pitch_offset_semi: 0.0,
        detune_cents: 0.0,
        formant_semi: 0.0,
        unvoiced: 0.0,
        drive: 0.0,
        mix,
        output_lin: 1.0,
        mode,
        detail: 1,
        bypassed: false,
    }
}

/// Where an impulse at `at` comes out, dry only (Mix 0), in `mode`.
fn dry_peak(mode: u32) -> usize {
    let n = 8192;
    let at = 700;
    let input: Vec<f32> = (0..n).map(|i| if i == at { 1.0 } else { 0.0 }).collect();
    let sc = vec![0.0f32; n];
    let (mut out_l, mut out_r) = (vec![0.0f32; n], vec![0.0f32; n]);
    let mut voc = Vocoder::new(SR);
    voc.process_stereo(&input, &input, &mut out_l, &mut out_r, &sc, &sc, &params(mode, 0.0));
    let peak = out_l.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).map(|(i, _)| i).unwrap();
    assert!(out_l[peak].abs() > 0.5, "dry impulse lost ({})", out_l[peak]);
    peak - at
}

#[test]
fn the_reported_latency_is_the_dry_delay_in_both_modes() {
    let voc = Vocoder::new(SR);
    let reported = voc.latency_samples() as usize;
    assert_eq!(reported, STFT_LATENCY);
    assert_eq!(dry_peak(MODE_CLASSIC), reported, "Classic dry delay");
    assert_eq!(dry_peak(MODE_SPECTRAL), reported, "Spectral dry delay");
}

#[test]
fn the_stft_delay_is_one_whole_window() {
    // The STFT processor itself, through the shared block: the wet path of
    // Spectral mode arrives exactly here, so the dry above lines up with it.
    let (n, hop) = (2048, 512);
    let mut p = superduper_synth_core::spectral::StftProcessor::new(SR, n, hop, 1);
    let mut out = Vec::new();
    for i in 0..4 * n {
        let x = if i == 1000 { 1.0 } else { 0.0 };
        out.push(p.process_sample(&[x], |ins, o| o.copy_from_slice(ins[0])));
    }
    let at = out.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).map(|(i, _)| i).unwrap();
    assert_eq!(at - 1000, STFT_LATENCY);
    assert_eq!(p.latency(), STFT_LATENCY);
}
