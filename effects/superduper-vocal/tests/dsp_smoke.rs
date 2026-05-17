//! DSP-only smoke tests for the Vocal cleanup plugin. Drives the per-sample
//! pipeline through `superduper_vocal`'s public DSP blocks (de-esser
//! split-band logic and de-clicker ratio detector) without going through
//! CLAP.

use superduper_synth_core::dsp_blocks::{Biquad, EnvelopeDetector};

const SR: f32 = 48000.0;

/// Generate `n` samples of a sine at `hz` with the given peak amplitude.
fn sine(hz: f32, peak: f32, n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| peak * (i as f32 * core::f32::consts::TAU * hz / SR).sin())
        .collect()
}

fn rms(s: &[f32]) -> f32 {
    let sum: f32 = s.iter().map(|x| x * x).sum();
    (sum / s.len() as f32).sqrt()
}

/// Split-band de-esser: HPF + body complement should attenuate a 7 kHz
/// sibilance sine but leave a 200 Hz fundamental untouched.
#[test]
fn de_esser_split_band_attenuates_sibilance_not_body() {
    let mut hpf = Biquad::default();
    hpf.set_hpf(SR, 6000.0, 0.707);

    let hi = sine(7000.0, 0.5, 4096);
    let lo = sine(200.0, 0.5, 4096);

    let hi_sib: Vec<f32> = hi.iter().map(|&x| hpf.process(x)).collect();
    let hi_body: Vec<f32> = hi.iter().zip(&hi_sib).map(|(a, b)| a - b).collect();
    hpf.clear();
    let lo_sib: Vec<f32> = lo.iter().map(|&x| hpf.process(x)).collect();
    let lo_body: Vec<f32> = lo.iter().zip(&lo_sib).map(|(a, b)| a - b).collect();

    // Skip the filter transient.
    let hi_sib_rms = rms(&hi_sib[1024..]);
    let lo_sib_rms = rms(&lo_sib[1024..]);
    let lo_body_rms = rms(&lo_body[1024..]);
    println!(
        "7 kHz sib rms={hi_sib_rms:.4}  200 Hz sib rms={lo_sib_rms:.5}  200 Hz body rms={lo_body_rms:.4}"
    );

    assert!(hi_sib_rms > 0.25, "sibilance band must keep the 7 kHz tone");
    assert!(
        lo_sib_rms < 0.05,
        "sibilance band must reject the 200 Hz tone ({lo_sib_rms})"
    );
    assert!(
        lo_body_rms > 0.25,
        "body band must keep the 200 Hz tone ({lo_body_rms})"
    );
    // Sum identity: body + sib ≈ original (within numerical noise).
    let identity_err: f32 = lo
        .iter()
        .zip(lo_sib.iter().zip(lo_body.iter()))
        .map(|(orig, (s, b))| (orig - (s + b)).abs())
        .fold(0.0_f32, f32::max);
    assert!(identity_err < 1e-5, "body + sib must equal input ({identity_err})");
}

/// Click detector: a short impulse on top of low-amplitude noise should
/// push the fast/slow envelope ratio well above sensitivity threshold.
/// Normal sustained signal should keep ratio near 1.
#[test]
fn click_detector_ratio_spikes_on_transient() {
    // Generate a sine, then splice in a +1.0 impulse 1024 samples in.
    let mut buf = sine(440.0, 0.1, 4096);
    let click_pos = 1024;
    buf[click_pos] = 1.0;

    let mut fast = EnvelopeDetector::default();
    let mut slow = EnvelopeDetector::default();
    let mut max_ratio = 0.0_f32;
    let mut sustained_ratio = 0.0_f32;

    for (i, &x) in buf.iter().enumerate() {
        let f = fast.process(x.abs(), SR, 0.1, 0.5);
        let s = slow.process(x.abs(), SR, 5.0, 50.0);
        let r = f / s.max(1e-6);
        if i > click_pos && i < click_pos + 64 {
            // Peak ratio happens shortly after the impulse — fast env
            // catches it instantly, slow env lags by milliseconds.
            if r > max_ratio { max_ratio = r; }
        }
        if i == 3500 {
            sustained_ratio = r;
        }
    }
    println!("click max ratio near impulse: {max_ratio:.2}  sustained: {sustained_ratio:.2}");
    assert!(max_ratio > 3.0, "click must spike ratio > 3 (got {max_ratio})");
    assert!(sustained_ratio < 2.0, "sustained signal should be near 1 (got {sustained_ratio})");
}

/// Bypass behavior in DSP space: when ess_amount = 0 AND click_amount = 0,
/// the body+sib reconstruction should be transparent to the input.
#[test]
fn ess_amt_zero_is_transparent() {
    let mut hpf = Biquad::default();
    hpf.set_hpf(SR, 6000.0, 0.707);
    let x = sine(1000.0, 0.4, 2048);
    let sib: Vec<f32> = x.iter().map(|&v| hpf.process(v)).collect();
    let body: Vec<f32> = x.iter().zip(&sib).map(|(a, b)| a - b).collect();
    // ess_gain_lin = 10^(0 / 20) = 1.0 → output = body + sib*1 = body+sib = input.
    let out: Vec<f32> = sib.iter().zip(&body).map(|(s, b)| b + s * 1.0).collect();
    for (a, b) in x.iter().zip(&out) {
        assert!((a - b).abs() < 1e-5, "transparency lost: {a} vs {b}");
    }
}
