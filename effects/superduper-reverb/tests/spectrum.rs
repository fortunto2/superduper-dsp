//! Spectrum-based tests for SuperDuper Reverb.
//!
//! Unlike `dsp_smoke.rs` which only validates RMS/peak/diff numerics, this
//! suite renders ASCII spectrograms and frequency-response curves so a
//! human (or an AI reading the test output) can SEE whether the reverb
//! sounds plausible — without ever opening REAPER.
//!
//! Run with: `cargo test -p superduper-reverb --test spectrum -- --nocapture`
//! (the `--nocapture` is important — without it the ASCII art is hidden.)

use superduper_reverb::{PlateParams, PlateState};
use superduper_synth_core::analysis::{
    ascii_spectrum, frequency_response_sine_sweep, log_freq_grid, spectrum_with_freq,
    AsciiSpectrumOpts,
};

const SR: f32 = 48_000.0;

fn default_params() -> PlateParams {
    PlateParams {
        sr: SR,
        size: 1.0,
        decay: 0.85,
        damp: 0.3,
        bandwidth: 0.85,
        predelay_ms: 0.0,
        modulation: 0.5,
    }
}

#[test]
fn impulse_response_tail_spectrum() {
    let mut state = PlateState::default();
    let p = default_params();

    // 2 s of audio: short impulse, then silence.
    let mut buf_l = vec![0.0_f32; (SR * 2.0) as usize];
    let mut buf_r = vec![0.0_f32; (SR * 2.0) as usize];
    for s in buf_l.iter_mut().take(8) { *s = 1.0; }
    for s in buf_r.iter_mut().take(8) { *s = 1.0; }
    let mut out_l: Vec<f32> = Vec::with_capacity(buf_l.len());
    let mut out_r: Vec<f32> = Vec::with_capacity(buf_r.len());
    for (l, r) in buf_l.iter().zip(buf_r.iter()) {
        let (wl, wr) = state.process_sample(*l, *r, p);
        out_l.push(wl);
        out_r.push(wr);
    }

    // FFT the middle 16384 samples of the tail (skip the impulse, get the
    // body of the reverb decay).
    let fft_len = 16384;
    let start = (SR * 0.3) as usize; // 300 ms in
    let window: Vec<f32> = (0..fft_len)
        .map(|i| (out_l[start + i] + out_r[start + i]) * 0.5)
        .collect();

    let spec = spectrum_with_freq(&window, SR);

    println!("\nReverb impulse-response tail spectrum (300 ms in, 16k window):");
    println!("{}", ascii_spectrum(&spec, &AsciiSpectrumOpts::default()));

    // Sanity: there must be some energy in the audible band. We don't
    // assert a shape (humans look at the picture) but we DO catch the
    // failure-mode where the spectrum is just floor.
    let any_audible = spec.iter().any(|&(f, db)| f > 100.0 && f < 8000.0 && db > -60.0);
    assert!(any_audible, "tail spectrum is entirely below -60 dB — reverb broken");
}

#[test]
fn damping_actually_rolls_off_highs() {
    // Compare tail spectra at damp=0.1 (bright) vs damp=0.9 (dark) — the
    // 8 kHz energy MUST be lower in the dark version.
    fn render_tail(damp: f32) -> Vec<(f32, f32)> {
        let mut state = PlateState::default();
        let p = PlateParams {
            sr: SR,
            size: 1.0,
            decay: 0.85,
            damp,
            bandwidth: 0.85,
            predelay_ms: 0.0,
            modulation: 0.4,
        };
        let mut input_l = vec![0.0_f32; (SR * 1.5) as usize];
        let mut input_r = vec![0.0_f32; (SR * 1.5) as usize];
        // Short white-noise burst to excite every band.
        let mut rng = 1u32;
        for i in 0..(SR as usize / 10) {
            rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let x = ((rng >> 16) as f32 / 32768.0 - 1.0) * 0.5;
            input_l[i] = x;
            input_r[i] = x;
        }
        let mut out_l = Vec::with_capacity(input_l.len());
        let mut out_r = Vec::with_capacity(input_r.len());
        for (l, r) in input_l.iter().zip(input_r.iter()) {
            let (wl, wr) = state.process_sample(*l, *r, p);
            out_l.push(wl);
            out_r.push(wr);
        }
        let fft_len = 16384;
        let start = (SR * 0.5) as usize;
        let window: Vec<f32> = (0..fft_len)
            .map(|i| (out_l[start + i] + out_r[start + i]) * 0.5)
            .collect();
        spectrum_with_freq(&window, SR)
    }

    let bright = render_tail(0.1);
    let dark = render_tail(0.9);

    fn band_db(spec: &[(f32, f32)], lo: f32, hi: f32) -> f32 {
        let in_band: Vec<f32> = spec.iter()
            .filter(|(f, _)| *f >= lo && *f <= hi)
            .map(|&(_, db)| db)
            .collect();
        if in_band.is_empty() { return -120.0; }
        // Mean dB in band (yes, mean of dB — fine for relative comparison).
        in_band.iter().sum::<f32>() / in_band.len() as f32
    }

    let bright_hi = band_db(&bright, 4000.0, 12_000.0);
    let dark_hi = band_db(&dark, 4000.0, 12_000.0);

    println!("\nDamping comparison — average 4–12 kHz energy in tail:");
    println!("  damp=0.1 → {:>6.1} dB", bright_hi);
    println!("  damp=0.9 → {:>6.1} dB", dark_hi);
    println!("\nbright (damp=0.1):");
    println!("{}", ascii_spectrum(&bright, &AsciiSpectrumOpts::default()));
    println!("\ndark (damp=0.9):");
    println!("{}", ascii_spectrum(&dark, &AsciiSpectrumOpts::default()));

    // High band must drop noticeably under heavy damping. 3 dB is a
    // conservative threshold — real plate damping easily cuts 10+ dB.
    assert!(
        dark_hi < bright_hi - 3.0,
        "damping doesn't roll off highs (bright={bright_hi:.1} dB, dark={dark_hi:.1} dB)",
    );
}

#[test]
fn frequency_response_curve() {
    let mut state = PlateState::default();
    let p = default_params();
    let freqs = log_freq_grid();

    // For each freq, run a tone through; measure RMS in vs RMS out on
    // the second half. With 100% wet the output is the reverb tail
    // (lots of gain in the audible band) — so we compare relative shape.
    let curve = frequency_response_sine_sweep(
        |x| {
            let (wl, _wr) = state.process_sample(x, x, p);
            wl
        },
        SR,
        &freqs,
        1.5,
    );

    println!("\nReverb frequency response (decay=0.85, damp=0.3):");
    let formatted: Vec<(f32, f32)> = curve.iter().copied().collect();
    println!("{}", ascii_spectrum(&formatted, &AsciiSpectrumOpts {
        min_db: -40.0,
        max_db: 10.0,
        ..Default::default()
    }));

    // Sanity — at least one mid band should pass within ±20 dB of unity.
    let mid_band: Vec<f32> = curve.iter()
        .filter(|(f, _)| *f >= 200.0 && *f <= 2000.0)
        .map(|&(_, g)| g)
        .collect();
    let avg_mid = mid_band.iter().sum::<f32>() / mid_band.len() as f32;
    println!("avg gain 200-2000 Hz = {:.1} dB", avg_mid);
    assert!(
        avg_mid > -30.0 && avg_mid < 30.0,
        "mid-band gain wildly off ({avg_mid} dB) — reverb level broken"
    );
}
