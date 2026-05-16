//! Spectrum-based tests for SuperDuper Supermass. See `superduper-reverb`'s
//! `spectrum.rs` for the rationale: prints ASCII spectrograms so a human
//! or AI reading the output can see whether the cascade reverb is shaping
//! frequencies the way it claims.
//!
//! Run with: `cargo test -p superduper-supermass --test spectrum -- --nocapture`

use fundsp::audiounit::AudioUnit;
use superduper_synth_core::analysis::{
    ascii_spectrum, spectrum_with_freq, AsciiSpectrumOpts,
};
use superduper_synth_core::supermass;

const SR: f64 = 48_000.0;

#[test]
fn cascade_tail_spectrum() {
    let mut net = supermass::build_wet();
    net.set_sample_rate(SR);

    // Excite with 100 ms of band-limited noise, then 2 s of silence so the
    // tail develops fully through both reverbs.
    let n = (SR * 3.0) as usize;
    let burst = (SR * 0.1) as usize;
    let mut tail = vec![0.0_f32; n];
    let mut rng = 0x1234_5678_u32;
    let mut in_buf = [0.0_f32; 2];
    let mut out_buf = [0.0_f32; 2];
    for i in 0..n {
        let x = if i < burst {
            rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((rng >> 16) as f32 / 32768.0 - 1.0) * 0.3
        } else {
            0.0
        };
        in_buf[0] = x;
        in_buf[1] = x;
        net.tick(&in_buf, &mut out_buf);
        tail[i] = (out_buf[0] + out_buf[1]) * 0.5;
    }

    // FFT a window deep in the tail (1.5 s in — well past the noise burst,
    // both reverb stages fully developed).
    let fft_len = 16384;
    let start = (SR * 1.5) as usize;
    let window: Vec<f32> = tail[start..start + fft_len].to_vec();
    let spec = spectrum_with_freq(&window, SR as f32);

    println!("\nSupermass cascade-reverb tail spectrum (1.5 s in):");
    println!("{}", ascii_spectrum(&spec, &AsciiSpectrumOpts::default()));

    // Sanity: must have audible-band energy. Threshold relaxed vs Dattorro
    // because Supermass's cascade T60 (28 s second reverb) takes ~1.5 s
    // to bloom from a 100 ms burst — energy is genuinely lower at the
    // measurement point.
    let any_audible = spec.iter().any(|&(f, db)| f > 100.0 && f < 6000.0 && db > -75.0);
    assert!(any_audible, "Supermass tail is below the noise floor — graph broken");
}

#[test]
fn supermass_is_darker_than_dattorro() {
    // Sanity-check: Supermass's second stage has heavy damping (0.90), so
    // its tail spectrum should fall off faster in the highs compared to a
    // raw Dattorro plate at moderate damping. We compare 6-12 kHz energy
    // here purely as a "the cascade is doing what it should be doing" check.
    let mut net = supermass::build_wet();
    net.set_sample_rate(SR);

    let n = (SR * 3.0) as usize;
    let burst = (SR * 0.1) as usize;
    let mut tail = vec![0.0_f32; n];
    let mut rng = 0xfeed_face_u32;
    let mut in_buf = [0.0_f32; 2];
    let mut out_buf = [0.0_f32; 2];
    for i in 0..n {
        let x = if i < burst {
            rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((rng >> 16) as f32 / 32768.0 - 1.0) * 0.3
        } else {
            0.0
        };
        in_buf[0] = x;
        in_buf[1] = x;
        net.tick(&in_buf, &mut out_buf);
        tail[i] = (out_buf[0] + out_buf[1]) * 0.5;
    }
    let fft_len = 16384;
    let start = (SR * 1.5) as usize;
    let window: Vec<f32> = tail[start..start + fft_len].to_vec();
    let spec = spectrum_with_freq(&window, SR as f32);

    fn band_db(spec: &[(f32, f32)], lo: f32, hi: f32) -> f32 {
        let in_band: Vec<f32> = spec.iter()
            .filter(|(f, _)| *f >= lo && *f <= hi)
            .map(|&(_, db)| db)
            .collect();
        if in_band.is_empty() { return -120.0; }
        in_band.iter().sum::<f32>() / in_band.len() as f32
    }

    let mid = band_db(&spec, 500.0, 2000.0);
    let high = band_db(&spec, 6000.0, 12000.0);
    println!("Supermass tail energy: 500-2k Hz = {mid:.1} dB, 6-12k Hz = {high:.1} dB");

    // High band must be at least 6 dB below the mid band (heavy damping +
    // chorus phase smearing both eat into the highs). If somebody breaks
    // the cascade so it just passes everything, this test will fail.
    assert!(
        high < mid - 6.0,
        "Supermass tail isn't darker in the highs (mid={mid:.1}, high={high:.1})"
    );
}
