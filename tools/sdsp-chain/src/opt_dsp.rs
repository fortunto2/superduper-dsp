//! Meters for `sdsp-chain opt`: the bus bands the plugins publish, masking shares between stems, the
//! level-free band balance, and the broadband drop. Port of the meter half of tools/mixopt.py.
//! AI-NOTE: masking mirrors synth-core/src/mixmap.rs + gui.rs::scope_spectrum (Hann 4096,
//! 16/(3N^2) Parseval, 32 log bands).

use std::collections::BTreeMap;

use realfft::RealFftPlanner;

use crate::check::{self, BANDS};

pub const BUS_BANDS: usize = 32;
pub const LOUD_DB: f64 = -50.0;
pub const CLOSE_DB: f64 = 6.0;
pub const LEVEL_FREE_DB: f64 = 2.0;
pub const N_FFT: usize = 4096;
pub const HOP: usize = 2048;

/// Python's round(): half to even.
fn round_even(x: f64) -> i64 {
    let r = x.round();
    if (x - x.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
        (r - x.signum()) as i64
    } else {
        r as i64
    }
}

/// Hann-windowed one-sided power spectra, one row per frame (a short signal is zero-padded to one frame).
pub fn frames(mono: &[f64], n: usize, hop: usize) -> Vec<Vec<f64>> {
    let mut x = mono.to_vec();
    if x.len() < n {
        x.resize(n, 0.0);
    }
    let win = check::hann(n);
    let fft = RealFftPlanner::<f64>::new().plan_fft_forward(n);
    let (mut buf, mut spec) = (fft.make_input_vec(), fft.make_output_vec());
    let mut out = vec![];
    let mut start = 0;
    while start + n <= x.len() {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = x[start + i] * win[i];
        }
        let _ = fft.process(&mut buf, &mut spec);
        out.push(spec.iter().map(|c| c.norm_sqr()).collect());
        start += hop;
    }
    out
}

/// 32 log bands 20 Hz ... 20 kHz (sdk bus::band_edges).
pub fn bus_band_edges() -> Vec<f64> {
    (0..=BUS_BANDS)
        .map(|b| 20.0 * 1000f64.powf(b as f64 / BUS_BANDS as f64))
        .collect()
}

/// Per frame, the 32 bus bands in dB of band RMS, the normalisation the plugins publish: Parseval with a
/// Hann window, rms^2 in a bin set = 16/(3N^2) * sum |X_k|^2, so a track's bands sum to its level.
pub fn bus_bands(mono: &[f64], sr: f64, n: usize, hop: usize) -> Vec<Vec<f64>> {
    let pw = frames(mono, n, hop);
    let bin_hz = sr / n as f64;
    let norm = 16.0 / (3.0 * (n * n) as f64);
    let edges = bus_band_edges();
    pw.iter()
        .map(|row| {
            (0..BUS_BANDS)
                .map(|b| {
                    let (lo, hi) = (edges[b] / bin_hz, edges[b + 1] / bin_hz);
                    let (i0, i1) = (lo.ceil() as usize, hi.ceil() as usize);
                    let p = if i1 > i0 {
                        row[i0.min(row.len())..i1.min(row.len())]
                            .iter()
                            .sum::<f64>()
                    } else {
                        let k = round_even(lo) as usize;
                        if k < row.len() {
                            row[k] * (hi - lo)
                        } else {
                            0.0
                        }
                    };
                    10.0 * (p * norm).max(1e-12).log10()
                })
                .collect()
        })
        .collect()
}

/// check's bands (sub ... top) as dB centred on their mean: a shape, so loudness stays its own term.
pub fn balance_bands(mono: &[f64], sr: f64) -> Vec<f64> {
    let rows = frames(mono, N_FFT, HOP);
    let mut pw = vec![0.0; N_FFT / 2 + 1];
    for r in &rows {
        pw.iter_mut().zip(r).for_each(|(a, b)| *a += b);
    }
    let v: Vec<f64> = BANDS
        .iter()
        .map(|(lo, hi, _)| {
            let s: f64 = pw
                .iter()
                .enumerate()
                .filter(|(k, _)| {
                    (*k as f64 / (N_FFT as f64 / sr)) >= *lo
                        && (*k as f64 / (N_FFT as f64 / sr)) < *hi
                })
                .map(|(_, p)| p)
                .sum();
            10.0 * s.max(1e-20).log10()
        })
        .collect();
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    v.iter().map(|x| x - mean).collect()
}

pub type Pair = (String, String);

/// {(a, b): 32-vector}: share of frames where both stems are > LOUD_DB in the band and within CLOSE_DB of each other.
pub fn masking_shares(stems: &BTreeMap<String, Vec<Vec<f64>>>) -> Vec<(Pair, Vec<f64>)> {
    let names: Vec<&String> = stems.keys().collect();
    let mut out = vec![];
    for (i, a) in names.iter().enumerate() {
        for b in &names[i + 1..] {
            let (p, q) = (&stems[*a], &stems[*b]);
            let n = p.len().min(q.len());
            let mut share = vec![0.0; BUS_BANDS];
            for t in 0..n {
                for k in 0..BUS_BANDS {
                    if p[t][k] > LOUD_DB
                        && q[t][k] > LOUD_DB
                        && (p[t][k] - q[t][k]).abs() < CLOSE_DB
                    {
                        share[k] += 1.0;
                    }
                }
            }
            if n > 0 {
                share.iter_mut().for_each(|s| *s /= n as f64);
            }
            out.push((((*a).clone(), (*b).clone()), share));
        }
    }
    out
}

pub struct MaskRow {
    pub a: String,
    pub b: String,
    pub hz: (i64, i64),
    pub worst_share: f64,
    pub total: f64,
}

/// Pairs sorted worst first: worst band (Hz) and its share, and the total.
pub fn masking_report(shares: &[(Pair, Vec<f64>)]) -> Vec<MaskRow> {
    let edges = bus_band_edges();
    let mut rows: Vec<MaskRow> = shares
        .iter()
        .map(|((a, b), s)| {
            let k = s
                .iter()
                .enumerate()
                .fold(0, |best, (i, v)| if *v > s[best] { i } else { best });
            MaskRow {
                a: a.clone(),
                b: b.clone(),
                hz: (round_even(edges[k]), round_even(edges[k + 1])),
                worst_share: s[k],
                total: s.iter().sum(),
            }
        })
        .collect();
    rows.sort_by(|x, y| {
        y.total
            .partial_cmp(&x.total)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rows
}

/// Time-averaged bus bands (dB of mean power), as mixmap.py averages them.
pub fn avg_bands(l: &[f64], r: &[f64], sr: f64) -> Vec<f64> {
    let mono: Vec<f64> = l.iter().zip(r).map(|(a, b)| (a + b) * 0.5).collect();
    let b = bus_bands(&mono, sr, N_FFT, HOP);
    (0..BUS_BANDS)
        .map(|k| {
            10.0 * (b.iter().map(|row| 10f64.powf(row[k] / 10.0)).sum::<f64>() / b.len() as f64
                + 1e-12)
                .log10()
        })
        .collect()
}

/// How much quieter a stem got as a whole: the median per-band drop over the bands that carry energy in
/// the base. A fader move drops every band and shows in full; a narrow bell moves a few and the median stays near 0.
pub fn broadband_drop(base: &[f64], now: &[f64]) -> f64 {
    let mut d: Vec<f64> = base
        .iter()
        .zip(now)
        .filter(|(b, _)| **b > -80.0)
        .map(|(b, n)| b - n)
        .collect();
    if d.is_empty() {
        return 0.0;
    }
    d.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let m = d.len() / 2;
    if d.len() % 2 == 1 {
        d[m]
    } else {
        (d[m - 1] + d[m]) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f64 = 48000.0;

    fn tone(hz: f64, amp: f64) -> Vec<f64> {
        (0..(4.0 * SR) as usize)
            .map(|i| amp * (2.0 * std::f64::consts::PI * hz * i as f64 / SR).sin())
            .collect()
    }

    #[test]
    fn bus_band_meter_reads_a_tone_at_its_rms() {
        // -20 dBFS RMS sine at 300 Hz: the 266-331 Hz bus band reads -20 dB, two bands up stays under the -50 dB threshold.
        let b = bus_bands(
            &tone(300.0, 10f64.powf(-20.0 / 20.0) * 2f64.sqrt()),
            SR,
            N_FFT,
            HOP,
        );
        let edges = bus_band_edges();
        let k = edges.iter().position(|e| *e > 300.0).unwrap() - 1;
        assert!(
            (265.0..=267.0).contains(&edges[k]) && (330.0..=332.0).contains(&edges[k + 1]),
            "{:?}",
            &edges[k..k + 2]
        );
        let mean = b.iter().map(|r| r[k]).sum::<f64>() / b.len() as f64;
        assert!((mean + 20.0).abs() < 0.3, "{mean}");
        assert!(b.iter().map(|r| r[k + 2]).fold(f64::MIN, f64::max) < -50.0);
    }

    #[test]
    fn broadband_drop_is_the_median_so_a_narrow_bell_is_free() {
        let base = vec![-20.0; 8];
        let mut bell = base.clone();
        bell[3] = -35.0;
        assert_eq!(broadband_drop(&base, &bell), 0.0);
        assert!(
            (broadband_drop(&base, &base.iter().map(|x| x - 5.0).collect::<Vec<_>>()) - 5.0).abs()
                < 1e-12
        );
        assert_eq!(broadband_drop(&[-120.0; 4], &[-130.0; 4]), 0.0);
    }

    #[test]
    fn two_close_loud_tones_clash_and_far_apart_ones_do_not() {
        let amp = 10f64.powf(-20.0 / 20.0) * 2f64.sqrt();
        let band = |x: &[f64]| bus_bands(x, SR, N_FFT, HOP);
        let mut stems = BTreeMap::new();
        stems.insert("a".to_string(), band(&tone(300.0, amp)));
        stems.insert("b".to_string(), band(&tone(305.0, amp)));
        let close = masking_report(&masking_shares(&stems));
        assert!(
            close[0].worst_share > 0.5 && (265..=267).contains(&close[0].hz.0),
            "{:?}",
            close[0].hz
        );
        stems.insert("b".to_string(), band(&tone(2000.0, amp)));
        assert!(masking_report(&masking_shares(&stems))[0].worst_share < 0.2);
    }

    #[test]
    fn balance_is_centred_whatever_the_level() {
        let m = balance_bands(&tone(1000.0, 0.3), SR);
        assert!(m.iter().sum::<f64>().abs() < 1e-9);
        let loud = balance_bands(&tone(1000.0, 0.6), SR);
        assert!(m.iter().zip(&loud).all(|(a, b)| (a - b).abs() < 1e-9));
    }
}
