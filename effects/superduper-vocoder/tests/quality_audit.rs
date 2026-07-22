//! Objective quality audit for the vocoder's internal carrier + drive.
//!
//! The band-limited (PolyBLEP) oscillators are the only part of this plugin
//! that can generate aliasing, so that's what we measure. Two batteries:
//!
//! 1. **Extreme-pitch aliasing** — a saw at 0.45·Nyquist. Almost all of a
//!    naive saw's harmonics live above Nyquist and fold back as aliasing;
//!    `measure_aliasing_db` reports the strongest folded image relative to
//!    the fundamental. PolyBLEP should keep it well down.
//! 2. **PolyBLEP vs naive** — at a musical mid pitch (non-integer period so
//!    aliasing lands *between* the real harmonics and is measurable), the
//!    inter-harmonic alias floor of the PolyBLEP saw must beat a naive saw
//!    by a wide margin. Proves the band-limiting actually works.
//!
//! Plus a THD sanity check on the `tanh` drive stage.
//!
//! Run: `cargo test -p superduper-vocoder --test quality_audit -- --nocapture`

use superduper_synth_core::analysis::{
    make_bin_aligned_sine, measure_aliasing_db, measure_thd_db, spectrum_with_freq,
};
use superduper_synth_core::dsp_blocks::tanh_drive;
use superduper_vocoder::dsp::{Carrier, WAVE_SAW};

const SR: f32 = 48_000.0;
const FFT_LEN: usize = 16384;

fn polyblep_saw(freq: f32, n: usize) -> Vec<f32> {
    let mut c = Carrier::default();
    // Detune 0 → both voices collapse to one; take the (identical) left channel.
    let (h, a, b, nrm) = Carrier::detune_pan(0.0, WAVE_SAW);
    (0..n).map(|_| c.next_stereo(WAVE_SAW, freq, h, a, b, nrm, SR).0).collect()
}

/// Naive (aliasing) saw for comparison — no band-limiting.
fn naive_saw(freq: f32, n: usize) -> Vec<f32> {
    let dt = freq / SR;
    let mut phase = 0.0f32;
    (0..n)
        .map(|_| {
            let y = 2.0 * phase - 1.0;
            phase += dt;
            if phase >= 1.0 {
                phase -= 1.0;
            }
            y
        })
        .collect()
}

/// Aliasing floor for a periodic-ish signal: FFT (Hann), find the
/// fundamental, then the loudest bin that is NOT within a small band of any
/// integer harmonic of `f0`. Returns that floor relative to the fundamental
/// in dB. Works at a *musical* pitch (unlike `measure_aliasing_db`, which
/// only masks the first 12 harmonics) as long as `SR/f0` is non-integer so
/// the folded images land off the harmonic grid.
fn inter_harmonic_alias_db(samples: &[f32], f0: f32) -> f32 {
    let spec = spectrum_with_freq(samples, SR);
    let fund_db = spec
        .iter()
        .filter(|(f, _)| (*f - f0).abs() < f0 * 0.05)
        .map(|&(_, d)| d)
        .fold(f32::NEG_INFINITY, f32::max);
    let mut floor = f32::NEG_INFINITY;
    for &(f, d) in &spec {
        if f < 60.0 || f > SR * 0.5 - 200.0 {
            continue;
        }
        let nearest = (f / f0).round();
        if nearest >= 1.0 && (f - nearest * f0).abs() < f0 * 0.03 {
            continue; // skip the real harmonic and its main lobe
        }
        if d > floor {
            floor = d;
        }
    }
    floor - fund_db
}

#[test]
fn carrier_saw_aliasing_extreme_pitch() {
    // 0.45 · Nyquist = 10.8 kHz. A naive saw here folds a wall of images
    // back into the band; PolyBLEP suppresses them.
    let f = 0.45 * SR * 0.5;
    let saw = polyblep_saw(f, FFT_LEN);
    let alias = measure_aliasing_db(&saw, f, SR);
    println!("\nPolyBLEP saw @ {f:.0} Hz — folded-image level: {alias:.1} dB (lower = cleaner)");

    // At this extreme pitch PolyBLEP is not perfect, but the strongest image
    // should sit well below the fundamental. -20 dB is a conservative gate;
    // in practice it measures a good deal lower. Any *musical* carrier pitch
    // is far cleaner than this worst case.
    assert!(alias < -20.0, "carrier aliasing too high at extreme pitch ({alias:.1} dB)");
}

#[test]
fn polyblep_beats_naive_saw() {
    // 2333 Hz → SR/f0 ≈ 20.6 (non-integer) so aliasing images land between
    // the real harmonics where we can measure them.
    let f = 2333.0;
    let poly = polyblep_saw(f, FFT_LEN);
    let naive = naive_saw(f, FFT_LEN);

    let poly_floor = inter_harmonic_alias_db(&poly, f);
    let naive_floor = inter_harmonic_alias_db(&naive, f);
    println!("\nInter-harmonic alias floor @ {f} Hz:");
    println!("  naive saw   : {naive_floor:.1} dB");
    println!("  PolyBLEP saw: {poly_floor:.1} dB");

    assert!(
        poly_floor < naive_floor - 8.0,
        "PolyBLEP should beat naive saw by >8 dB (naive={naive_floor:.1}, poly={poly_floor:.1})"
    );
}

#[test]
fn drive_stage_generates_sane_harmonics() {
    // The Drive control is `tanh_drive` on the summed vocoded signal. Feed a
    // clean bin-aligned 1 kHz sine through it at a moderate drive and check
    // the harmonic content is measurable but not insane.
    let (sine, freq) = make_bin_aligned_sine(FFT_LEN, SR, 1000.0, 0.7);
    // Drive param 0.5 → drive_lin = 1 + 0.5·6 = 4 (matches dsp.rs mapping).
    let driven: Vec<f32> = sine.iter().map(|&x| tanh_drive(x * 2.2, 4.0)).collect();
    let thd = measure_thd_db(&driven, freq, SR);
    println!("\nDrive stage THD @ 1 kHz (drive=0.5): {thd:.1} dB");
    assert!(thd > -60.0, "drive produced no harmonics ({thd:.1} dB) — tanh not saturating");
    assert!(thd < 0.0, "drive THD implausibly high ({thd:.1} dB)");
}
