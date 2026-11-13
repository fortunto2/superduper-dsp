//! Mono band-limited resampler (Kaiser-windowed sinc). Pure Rust; the model wants 22050 Hz.
//!
//! Direct evaluation per output sample with a tabulated kernel: ~100 taps at 48 kHz -> 22.05 kHz,
//! about 5 million multiply-adds per minute of audio.

const ZEROS: f64 = 24.0; // sinc zero crossings on each side of the kernel
const RES: usize = 512; // kernel table entries per input sample
const BETA: f64 = 9.0; // Kaiser beta: ~ -90 dB stop band

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, q) = (1.0, 1.0, x * x / 4.0);
    for k in 1..60 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < 1e-14 * sum {
            break;
        }
    }
    sum
}

/// Resample `x` from `in_sr` to `out_sr`. Output length is `ceil(len * out_sr / in_sr)`, like librosa.
pub fn resample(x: &[f32], in_sr: u32, out_sr: u32) -> Vec<f32> {
    if in_sr == out_sr {
        return x.to_vec();
    }
    let ratio = in_sr as f64 / out_sr as f64; // input samples per output sample
    // Low-pass at the lower of the two Nyquists, a little inside it.
    let scale = (1.0 / ratio).min(1.0) * 0.98;
    let half = ZEROS / scale; // kernel half-width in input samples
    let n_tab = (half * RES as f64).ceil() as usize + 2;
    let i0_beta = bessel_i0(BETA);
    let table: Vec<f32> = (0..n_tab)
        .map(|j| {
            let t = j as f64 / RES as f64;
            let a = t * scale;
            let sinc = if a < 1e-12 { 1.0 } else { (std::f64::consts::PI * a).sin() / (std::f64::consts::PI * a) };
            let r = (t / half).min(1.0);
            let w = bessel_i0(BETA * (1.0 - r * r).max(0.0).sqrt()) / i0_beta;
            (scale * sinc * w) as f32
        })
        .collect();
    // librosa's length: ceil(len * out / in), in integers
    let n_out = (x.len() as u64 * out_sr as u64).div_ceil(in_sr as u64) as usize;
    let mut out = Vec::with_capacity(n_out);
    for n in 0..n_out {
        let c = n as f64 * ratio;
        let lo = (c - half).ceil().max(0.0) as usize;
        let hi = ((c + half).floor() as usize).min(x.len().saturating_sub(1));
        let mut acc = 0.0f32;
        for (k, &xv) in x.iter().enumerate().take(hi + 1).skip(lo) {
            let d = (k as f64 - c).abs() * RES as f64;
            let j = d as usize;
            let f = (d - j as f64) as f32;
            let h = table[j] * (1.0 - f) + table[j + 1] * f;
            acc += xv * h;
        }
        out.push(acc);
    }
    out
}

/// Interleaved or planar is the caller's business: this averages equal-length channels.
pub fn mixdown(channels: &[&[f32]]) -> Vec<f32> {
    let n = channels.iter().map(|c| c.len()).min().unwrap_or(0);
    let k = channels.len().max(1) as f32;
    (0..n).map(|i| channels.iter().map(|c| c[i]).sum::<f32>() / k).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(f: f64, sr: u32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * std::f64::consts::PI * f * i as f64 / sr as f64).sin() as f32).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn passband_sine_keeps_level_and_length() {
        let y = resample(&sine(1000.0, 48000, 48000), 48000, 22050);
        assert_eq!(y.len(), 22050);
        let r = rms(&y[2000..20000]);
        assert!((r - 0.7071).abs() < 0.005, "rms {r}");
        // phase/frequency: zero crossings of a 1 kHz tone, 22050 Hz -> 44.1 samples per period
        let want = sine(1000.0, 22050, 22050);
        let err = y[2000..20000].iter().zip(&want[2000..20000]).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(err < 0.01, "max error {err}");
    }

    #[test]
    fn above_new_nyquist_is_removed() {
        let y = resample(&sine(15000.0, 48000, 48000), 48000, 22050);
        assert!(rms(&y[2000..20000]) < 0.003, "alias rms {}", rms(&y[2000..20000]));
    }

    #[test]
    fn upsampling_works_too() {
        let y = resample(&sine(1000.0, 16000, 16000), 16000, 22050);
        assert_eq!(y.len(), 22050);
        assert!((rms(&y[2000..20000]) - 0.7071).abs() < 0.005);
    }
}
