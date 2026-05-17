//! DSP smoke tests for SuperDuper Delay. Test the shared DelayLine
//! (Lagrange-3 interpolation) and SlewLimiter2Pole directly — that's
//! where the algorithm work lives. The plugin itself just wires them up.

use superduper_synth_core::dsp_blocks::{DelayLine, OnePoleLp, SlewLimiter2Pole};

const SR: f32 = 48_000.0;

#[test]
fn delay_line_integer_delay_is_exact() {
    // For integer delay values, Lagrange-3 must reproduce the sample
    // exactly (Lagrange interpolation passes through the support points).
    let mut d = DelayLine::new(8192);
    // Feed an impulse-train of recognisable values.
    let n = 1000;
    for i in 0..n {
        d.write((i as f32 * 0.01).sin());
    }
    // Read back at integer delay = the most recent sample.
    let last_written = ((n - 1) as f32 * 0.01).sin();
    let read = d.read_lagrange3(1.0);
    println!("integer-delay read = {read:.6}, expected = {last_written:.6}");
    assert!(
        (read - last_written).abs() < 1e-3,
        "integer-tap Lagrange off by {}",
        (read - last_written).abs()
    );
}

#[test]
fn delay_line_preserves_sine_amplitude() {
    // A 1 kHz sine fed through the delay should come out with the same
    // RMS as the input — no significant high-shelf cut from interpolation.
    let mut d = DelayLine::new(8192);
    let mut rms_in = 0.0_f32;
    let mut rms_out = 0.0_f32;
    let mut n = 0;
    // Warm up: fill the buffer with 0.5 seconds of sine.
    for i in 0..(SR as usize / 2) {
        let phase = i as f32 * 2.0 * core::f32::consts::PI * 1000.0 / SR;
        let x = phase.sin();
        d.write(x);
    }
    // Now measure: input vs delayed read at 100.5 samples (worst-case fractional).
    for i in 0..2048 {
        let phase = i as f32 * 2.0 * core::f32::consts::PI * 1000.0 / SR;
        let x = phase.sin();
        d.write(x);
        let y = d.read_lagrange3(100.5);
        rms_in += x * x;
        rms_out += y * y;
        n += 1;
    }
    let rms_in = (rms_in / n as f32).sqrt();
    let rms_out = (rms_out / n as f32).sqrt();
    let ratio_db = 20.0 * (rms_out / rms_in.max(1e-9)).log10();
    println!("1 kHz through Lagrange-3 (frac=0.5): {ratio_db:.3} dB");
    // Lagrange-3 ≤ 0.05 dB cut at 1 kHz / 48k. We allow ±0.5 dB to be safe.
    assert!(ratio_db.abs() < 0.5, "Lagrange-3 deviates {ratio_db} dB at 1 kHz");
}

#[test]
fn slew_limiter_smooth_step() {
    // SlewLimiter2Pole should rise monotonically without overshoot toward
    // the target. 30 ms time constant at 48k → ~99 % settled after 5×30 ms.
    let mut s = SlewLimiter2Pole::new(0.0);
    let mut prev = 0.0_f32;
    let mut over = false;
    for _ in 0..(SR as usize / 5) {
        // 200 ms
        let v = s.step(1.0, SR, 30.0);
        if v > 1.0 + 1e-3 { over = true; }
        if v < prev - 1e-5 { panic!("non-monotonic: prev={prev}, v={v}"); }
        prev = v;
    }
    println!("slew final = {prev:.4}");
    assert!(!over, "slew overshot 1.0");
    assert!(prev > 0.98, "slew didn't reach target ({prev})");
}

#[test]
fn one_pole_lp_cuts_high_band() {
    // Pump high-frequency content through, verify RMS reduces.
    let mut lp = OnePoleLp::default();
    let mut rms_in = 0.0_f32;
    let mut rms_out = 0.0_f32;
    let mut rng = 0xdead_beef_u32;
    let cutoff = 1000.0;
    // Throw away first 4k samples (settle).
    for _ in 0..4096 {
        rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let x = ((rng >> 16) as f32 / 32768.0 - 1.0) * 0.5;
        lp.process(x, SR, cutoff);
    }
    for _ in 0..16384 {
        rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let x = ((rng >> 16) as f32 / 32768.0 - 1.0) * 0.5;
        let y = lp.process(x, SR, cutoff);
        rms_in += x * x;
        rms_out += y * y;
    }
    let ratio = (rms_out / rms_in).sqrt();
    println!("white noise through 1-pole LP @ 1 kHz: ratio = {ratio:.4}");
    // White noise → 1-pole LP should drop RMS to roughly ~sqrt(cutoff/nyquist).
    assert!(ratio < 0.5, "1-pole LP barely attenuated noise (ratio={ratio})");
}
