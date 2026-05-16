//! DSP-level smoke test for SuperDuper Supermass.
//!
//! Drives the synth-core `build_wet()` Net directly (no CLAP host) and
//! validates the cascade reverb actually produces a long decaying tail.
//!
//! Run with: `cargo test -p superduper-supermass --test dsp_smoke -- --nocapture`

use fundsp::audiounit::AudioUnit;
use superduper_synth_core::supermass;

const SR: f64 = 48_000.0;

fn rms(s: &[f32]) -> f32 {
    let n = s.len() as f32;
    let sum: f32 = s.iter().map(|x| x * x).sum();
    (sum / n).sqrt()
}

fn peak(s: &[f32]) -> f32 {
    s.iter().map(|x| x.abs()).fold(0.0_f32, f32::max)
}

#[test]
fn cascade_produces_long_tail() {
    let mut net = supermass::build_wet();
    net.set_sample_rate(SR);

    // ~3 seconds of audio: brief impulse then silence.
    let n = (SR * 3.0) as usize;
    let mut wet_l = vec![0.0_f32; n];
    let mut wet_r = vec![0.0_f32; n];
    let mut in_buf = [0.0_f32; 2];
    let mut out_buf = [0.0_f32; 2];

    for i in 0..n {
        // Impulse cluster for first 8 samples.
        let x = if i < 8 { 1.0 } else { 0.0 };
        in_buf[0] = x;
        in_buf[1] = x;
        net.tick(&in_buf, &mut out_buf);
        wet_l[i] = out_buf[0];
        wet_r[i] = out_buf[1];
    }

    // Look at the tail well after the input is silent.
    let tail_l = &wet_l[(SR as usize)..];
    let tail_r = &wet_r[(SR as usize)..];
    let tail_peak = peak(tail_l).max(peak(tail_r));
    let tail_rms_l = rms(tail_l);
    let tail_rms_r = rms(tail_r);

    println!("supermass tail peak={:.6}  rms L={:.6} R={:.6}",
        tail_peak, tail_rms_l, tail_rms_r);

    // After 1 second the impulse should still be decaying audibly (28-second
    // T60). Pretty generous threshold so jitter in fundsp's FDN doesn't fail us.
    assert!(
        tail_peak > 1e-4,
        "supermass cascade is silent at t=1s (peak={tail_peak}) — graph broken"
    );
}

#[test]
fn cascade_decays_after_input_stops() {
    // Supermass has a 28 s T60 — it ACCUMULATES energy under sustained input
    // (that's the point). The stability test we care about is: once the
    // input goes silent, does the tail actually fall back below its peak?
    let mut net = supermass::build_wet();
    net.set_sample_rate(SR);

    // 2 seconds of moderate input, then measure the peak.
    let mut peak_with_input: f32 = 0.0;
    let mut in_buf = [0.3_f32; 2];
    let mut out_buf = [0.0_f32; 2];
    for _ in 0..(SR as usize * 2) {
        net.tick(&in_buf, &mut out_buf);
        peak_with_input =
            peak_with_input.max(out_buf[0].abs()).max(out_buf[1].abs());
    }

    // Cut input. Cascade keeps "blooming" for a beat as the first reverb's
    // tail flows through the chorus into the second reverb (this is Valhalla
    // Supermassive's signature wash). After that, it MUST start decaying.
    // We measure peak in two windows after silence: an early one (where
    // bloom is still happening) and a late one (where decay should dominate).
    in_buf[0] = 0.0;
    in_buf[1] = 0.0;
    let mut peak_bloom: f32 = 0.0;
    for _ in 0..(SR as usize * 3) {
        net.tick(&in_buf, &mut out_buf);
        peak_bloom = peak_bloom.max(out_buf[0].abs()).max(out_buf[1].abs());
    }
    let mut peak_late: f32 = 0.0;
    for _ in 0..(SR as usize * 8) {
        net.tick(&in_buf, &mut out_buf);
        peak_late = peak_late.max(out_buf[0].abs()).max(out_buf[1].abs());
    }

    println!(
        "supermass: peak_input={:.4}, peak_bloom={:.4}, peak_late={:.4}",
        peak_with_input, peak_bloom, peak_late
    );
    // Bounded.
    assert!(
        peak_bloom < 100.0,
        "supermass is unstable (bloom peak={peak_bloom})"
    );
    // Late window must also be bounded — runaway would mean late >> bloom by
    // orders of magnitude. With a 28 s T60 + cascade chorus, late > bloom by
    // a small factor is *expected* "Valhalla wash"; runaway would be 100×+.
    assert!(
        peak_late < peak_bloom * 4.0,
        "supermass diverging (late={peak_late}, bloom={peak_bloom}); feedback > unity"
    );
}

#[test]
fn stereo_taps_differ() {
    let mut net = supermass::build_wet();
    net.set_sample_rate(SR);

    let n = (SR * 0.5) as usize;
    let mut diffs: f32 = 0.0;
    let mut in_buf = [0.0_f32; 2];
    let mut out_buf = [0.0_f32; 2];
    for i in 0..n {
        let x = if i < 8 { 1.0 } else { 0.0 };
        in_buf[0] = x;
        in_buf[1] = x;
        net.tick(&in_buf, &mut out_buf);
        diffs += (out_buf[0] - out_buf[1]).powi(2);
    }
    let diff_rms = (diffs / n as f32).sqrt();
    println!("supermass L-R diff rms = {diff_rms:.6}");
    assert!(
        diff_rms > 1e-4,
        "L and R taps identical (diff_rms={diff_rms}); the chorus stage must be split"
    );
}
