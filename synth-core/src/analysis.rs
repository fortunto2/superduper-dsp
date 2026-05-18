//! Spectrum / frequency-response analysis helpers — purpose-built so the
//! test suite (and Claude reading test output) can see *what* the DSP is
//! doing, not just the RMS / peak / diff_rms numerics.
//!
//! Two main building blocks:
//!   - `magnitude_spectrum_db` — windowed real-FFT → dB-magnitude per bin
//!   - `ascii_spectrum`         — render a magnitude curve as a multi-row
//!     ASCII bar chart that prints legibly in a 100-column test log
//!
//! Optional sugar:
//!   - `frequency_response_sine_sweep` — feed a logarithmic sine sweep
//!     through a closure-shaped DSP block and recover its in-band gain
//!     curve. Useful for verifying reverb damping / EQ tilt behave like
//!     you expect.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;
use std::sync::Mutex;

// ---------------------------------------------------------------------------
// Plan caching — RealFftPlanner allocates on every plan() call. Tests run
// many times and want the same FFT size repeatedly; cache by size.
// ---------------------------------------------------------------------------

static PLAN_CACHE: Mutex<Option<RealFftPlanner<f32>>> = Mutex::new(None);

fn fft_forward(n: usize) -> std::sync::Arc<dyn realfft::RealToComplex<f32>> {
    let mut guard = PLAN_CACHE.lock().unwrap();
    let planner = guard.get_or_insert_with(RealFftPlanner::<f32>::new);
    planner.plan_fft_forward(n)
}

// ---------------------------------------------------------------------------
// Windowing — Hann window flattens leakage at the edges so the spectrum
// isn't dominated by a sinc-shaped peak from the rectangular cut.
// ---------------------------------------------------------------------------

pub fn hann_window(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            0.5 - 0.5
                * ((2.0 * core::f32::consts::PI * i as f32) / (n - 1) as f32).cos()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Magnitude spectrum in dB.
//
// Returns `n/2 + 1` bins covering 0 .. nyquist. Bin `k` maps to frequency
// `k * sr / n`. Values are clamped to ≥ -120 dB so a single zero sample
// doesn't blow up the ASCII renderer with -infinity.
// ---------------------------------------------------------------------------

pub fn magnitude_spectrum_db(samples: &[f32]) -> Vec<f32> {
    let n = samples.len();
    let mut input: Vec<f32> = hann_window(n)
        .iter()
        .zip(samples.iter())
        .map(|(w, s)| w * s)
        .collect();
    let fft = fft_forward(n);
    let mut output = fft.make_output_vec();
    fft.process(&mut input, &mut output).unwrap();

    // Coherent gain of the Hann window is 0.5 — scale by 1/(0.5*N) to
    // bring back to per-sample magnitude.
    let gain = 1.0 / (0.5 * n as f32);
    output
        .iter()
        .map(|c| {
            let mag = (c.norm()) * gain;
            let db = 20.0 * (mag + 1e-12).log10();
            db.max(-120.0)
        })
        .collect()
}

/// Pair magnitude bins with their frequency for easier ASCII rendering.
pub fn spectrum_with_freq(samples: &[f32], sr: f32) -> Vec<(f32, f32)> {
    let mag = magnitude_spectrum_db(samples);
    let n = samples.len() as f32;
    mag.into_iter()
        .enumerate()
        .map(|(i, db)| (i as f32 * sr / n, db))
        .collect()
}

// ---------------------------------------------------------------------------
// ASCII rendering.
// ---------------------------------------------------------------------------

pub struct AsciiSpectrumOpts {
    pub rows: usize,
    pub cols: usize,
    /// Y-axis range in dB. Anything below `min_db` is treated as floor.
    pub min_db: f32,
    pub max_db: f32,
    /// Frequency range to display (Hz).
    pub min_hz: f32,
    pub max_hz: f32,
    /// Logarithmic frequency axis (true) or linear (false).
    pub log_freq: bool,
}

impl Default for AsciiSpectrumOpts {
    fn default() -> Self {
        Self {
            rows: 16,
            cols: 80,
            min_db: -80.0,
            max_db: 0.0,
            min_hz: 20.0,
            max_hz: 20_000.0,
            log_freq: true,
        }
    }
}

/// Render a magnitude spectrum as ASCII art. Rows top-to-bottom go from
/// `max_db` (loud) down to `min_db` (quiet). Columns left-to-right span
/// `min_hz` .. `max_hz`. Bin energy in each column is the MAX of bins
/// that fall into that column's frequency range (peak-hold-style).
pub fn ascii_spectrum(spectrum: &[(f32, f32)], opts: &AsciiSpectrumOpts) -> String {
    let mut col_max: Vec<f32> = vec![opts.min_db; opts.cols];

    let log_min = opts.min_hz.max(1e-3).ln();
    let log_max = opts.max_hz.ln();

    for &(f, db) in spectrum {
        if f < opts.min_hz || f > opts.max_hz {
            continue;
        }
        let frac = if opts.log_freq {
            (f.max(1e-3).ln() - log_min) / (log_max - log_min)
        } else {
            (f - opts.min_hz) / (opts.max_hz - opts.min_hz)
        };
        let col = ((frac * opts.cols as f32) as usize).min(opts.cols - 1);
        if db > col_max[col] {
            col_max[col] = db;
        }
    }

    let mut out = String::with_capacity(opts.rows * (opts.cols + 16));
    for row in 0..opts.rows {
        // Y-axis label every few rows.
        let row_db = opts.max_db
            - (row as f32 / (opts.rows - 1) as f32) * (opts.max_db - opts.min_db);
        out.push_str(&format!("{:>5.0} dB |", row_db));

        for col in 0..opts.cols {
            let bar = if col_max[col] >= row_db {
                '#'
            } else if col_max[col] >= row_db - (opts.max_db - opts.min_db) / opts.rows as f32 {
                '.'
            } else {
                ' '
            };
            out.push(bar);
        }
        out.push('\n');
    }
    // X-axis ticks.
    out.push_str("        +");
    out.push_str(&"-".repeat(opts.cols));
    out.push('\n');
    let n_ticks = 7;
    out.push_str("         ");
    for tick in 0..n_ticks {
        let frac = tick as f32 / (n_ticks - 1) as f32;
        let freq = if opts.log_freq {
            (log_min + frac * (log_max - log_min)).exp()
        } else {
            opts.min_hz + frac * (opts.max_hz - opts.min_hz)
        };
        let label = if freq >= 1000.0 {
            format!("{:.0}k", freq / 1000.0)
        } else {
            format!("{:.0}", freq)
        };
        let target = (frac * (opts.cols - 1) as f32) as usize;
        let current = tick * opts.cols / n_ticks;
        let pad = target.saturating_sub(current);
        out.push_str(&" ".repeat(pad.min(opts.cols)));
        out.push_str(&label);
    }
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Frequency response via sine sweep.
//
// Probes the DSP with a logarithmically-swept sine wave, then measures
// envelope amplitude at each test frequency. Returns (freq_hz, gain_db).
//
// `process_one` is a closure that takes a mono sample and returns one
// processed sample. For per-block DSP (like the Net-based supermass) wrap
// it as `|x| { let mut o = [0.0; 1]; net.tick(&[x], &mut o); o[0] }`.
// ---------------------------------------------------------------------------

pub fn frequency_response_sine_sweep<F: FnMut(f32) -> f32>(
    mut process_one: F,
    sr: f32,
    freqs: &[f32],
    tone_seconds: f32,
) -> Vec<(f32, f32)> {
    let mut out = Vec::with_capacity(freqs.len());
    let n = (sr * tone_seconds) as usize;
    for &freq in freqs {
        // Reset DSP state isn't possible from outside the closure, so we
        // discard the first half of the buffer (transient) and measure RMS
        // on the second half.
        let mut sum_sq_in = 0.0_f32;
        let mut sum_sq_out = 0.0_f32;
        for i in 0..n {
            let x = (i as f32 * 2.0 * core::f32::consts::PI * freq / sr).sin();
            let y = process_one(x);
            if i >= n / 2 {
                sum_sq_in += x * x;
                sum_sq_out += y * y;
            }
        }
        let rms_in = (sum_sq_in / (n / 2) as f32).sqrt();
        let rms_out = (sum_sq_out / (n / 2) as f32).sqrt();
        let gain_db = 20.0 * ((rms_out / rms_in.max(1e-9)).max(1e-9)).log10();
        out.push((freq, gain_db));
    }
    out
}

/// Standard log-spaced frequency grid for sine-sweep tests: 1/3-octave
/// from 20 Hz to ~16 kHz (24 points).
pub fn log_freq_grid() -> Vec<f32> {
    let mut g = Vec::new();
    let mut f = 20.0_f32;
    while f <= 20_000.0 {
        g.push(f);
        f *= 2.0_f32.powf(1.0 / 3.0);
    }
    g
}

// ===========================================================================
// Objective quality measurements — PluginDoctor / Plugin Analyser style.
//
// Each one runs a single FFT on a chunk of processed audio and pulls a
// specific number out of the spectrum. They're written so per-plugin
// dsp_smoke tests can assert "THD better than -50 dB at 1 kHz", "aliasing
// rejection > 60 dB at 18 kHz with 4× oversampling", etc.
//
// FFT-based metrics have a noise floor set by:
//   - window length (longer = lower floor),
//   - the Hann window we use (–32 dB sidelobes),
//   - any other spectral content in the captured chunk.
//
// For meaningful THD tests, feed an integer number of cycles of a single
// pure sine, take ≥ 8192 samples, and `freq` should land on an FFT bin
// (i.e. choose `freq = sr * k / fft_len` for some integer k) to avoid
// bin leakage spreading the fundamental into its neighbours.
// ===========================================================================

/// Find the bin index of the strongest spectral peak. Used by THD /
/// aliasing measurements as a "where's the fundamental?" detector.
fn peak_bin(magnitudes_db: &[f32], skip_dc_bins: usize) -> usize {
    let mut max_db = f32::NEG_INFINITY;
    let mut max_i = 0;
    for (i, &db) in magnitudes_db.iter().enumerate().skip(skip_dc_bins) {
        if db > max_db {
            max_db = db;
            max_i = i;
        }
    }
    max_i
}

/// Sum the linear energy across bins `[lo..hi]` (inclusive lo, exclusive hi)
/// from a magnitude-dB spectrum. Returns dB. Reserved for future per-band
/// quality measurements (LF/HF balance, transition-band ripple, etc).
#[allow(dead_code)]
fn band_energy_db(magnitudes_db: &[f32], lo: usize, hi: usize) -> f32 {
    let hi = hi.min(magnitudes_db.len());
    if lo >= hi {
        return -120.0;
    }
    let mut sum_lin_sq = 0.0_f32;
    for &db in &magnitudes_db[lo..hi] {
        // dB → linear → energy
        let lin = 10f32.powf(db / 20.0);
        sum_lin_sq += lin * lin;
    }
    10.0 * sum_lin_sq.max(1e-24).log10()
}

/// Measure THD (total harmonic distortion) of a chunk of audio relative
/// to its fundamental. Returns the value in dB **below the fundamental**
/// (negative number = clean; -∞ = pure sine).
///
/// `samples` should contain a steady-state sine. Pure sine → ≈ -∞ dB,
/// pure square → about -10 dB (lots of harmonics), tanh-saturated sine
/// at unity drive → typically -40 to -50 dB.
///
/// Algorithm:
///   1. Hann-window the chunk, run real FFT.
///   2. Find the peak bin → that's the fundamental.
///   3. Sum energy in the 2nd–8th harmonic bins (with ±3 bin tolerance
///      for window leakage).
///   4. THD_dB = 20·log10(sqrt(harmonics_lin² / fundamental_lin²)).
pub fn measure_thd_db(samples: &[f32], _fundamental_hz: f32, _sr: f32) -> f32 {
    let mag = magnitude_spectrum_db(samples);
    let n_bins = mag.len();
    let fund_bin = peak_bin(&mag, 2);
    if fund_bin == 0 {
        return 0.0;
    }
    let fund_lin = 10f32.powf(mag[fund_bin] / 20.0);

    // Sum the linear amplitudes of harmonic peaks (2nd through 8th),
    // each searched in a ±3-bin window for window leakage.
    let mut harm_lin_sq = 0.0_f32;
    for h in 2..=8 {
        let centre = fund_bin * h;
        if centre + 3 >= n_bins {
            break;
        }
        let lo = centre.saturating_sub(3);
        let hi = (centre + 4).min(n_bins);
        let mut peak_db = f32::NEG_INFINITY;
        for &db in &mag[lo..hi] {
            if db > peak_db { peak_db = db; }
        }
        let lin = 10f32.powf(peak_db / 20.0);
        harm_lin_sq += lin * lin;
    }
    if harm_lin_sq <= 0.0 {
        return -200.0;
    }
    let thd_ratio = harm_lin_sq.sqrt() / fund_lin.max(1e-12);
    20.0 * thd_ratio.max(1e-20).log10()
}

/// Measure the aliasing floor: feed a known fundamental, run device, FFT,
/// then look at every bin that **isn't** the fundamental or an integer
/// harmonic. The max such bin's magnitude — relative to fundamental — is
/// the aliasing level in dB. Lower = cleaner.
///
/// For a saturator test, pick a fundamental near (but below) Nyquist so
/// every "real" harmonic is above Nyquist and folds back as aliasing.
/// `0.45 × sr` is the standard choice — for 48 kHz that's 21.6 kHz.
pub fn measure_aliasing_db(samples: &[f32], _fundamental_hz: f32, _sr: f32) -> f32 {
    let mag = magnitude_spectrum_db(samples);
    let n_bins = mag.len();
    let fund_bin = peak_bin(&mag, 2);
    if fund_bin == 0 {
        return 0.0;
    }
    let fund_db = mag[fund_bin];

    // Mask out the fundamental and its integer harmonics with ±3 bin halo.
    let mut allowed = vec![true; n_bins];
    for i in 0..6 { // DC + first 5 bins are always windowing noise
        allowed[i] = false;
    }
    for h in 1..=12 {
        let centre = fund_bin * h;
        if centre >= n_bins { break; }
        for off in -3..=3 {
            let bin = centre as i64 + off;
            if bin >= 0 && (bin as usize) < n_bins {
                allowed[bin as usize] = false;
            }
        }
    }

    // Find loudest "wrong" bin.
    let mut max_db = -200.0_f32;
    for (i, &db) in mag.iter().enumerate() {
        if allowed[i] && db > max_db {
            max_db = db;
        }
    }
    max_db - fund_db
}

/// SMPTE-style IMD: 60 Hz + 7 kHz at 4:1 amplitude ratio. Measures the
/// strongest sum/difference product around the high-frequency tone
/// (7060 / 6940 / 13940 / 14060 Hz) relative to the 7 kHz fundamental.
/// Used to characterise non-linearities under mixed input.
///
/// Caller is responsible for generating the input signal via
/// `imd_smpte_input(n, sr)` and feeding it through the device under test.
/// Returns dB.
pub fn measure_imd_smpte_db(samples: &[f32], sr: f32) -> f32 {
    let mag = magnitude_spectrum_db(samples);
    let n_bins = mag.len();
    let n = samples.len() as f32;

    let bin_hz = sr / (samples.len() as f32);
    let to_bin = |hz: f32| ((hz / bin_hz).round() as usize).min(n_bins - 1);
    let _ = n;

    let peak_in_range = |centre_hz: f32| -> f32 {
        let centre = to_bin(centre_hz);
        let lo = centre.saturating_sub(3);
        let hi = (centre + 4).min(n_bins);
        let mut peak_db = f32::NEG_INFINITY;
        for &db in &mag[lo..hi] {
            if db > peak_db { peak_db = db; }
        }
        peak_db
    };

    let fund_db = peak_in_range(7000.0);
    let mut max_imd = -200.0_f32;
    for hz in [6940.0_f32, 7060.0, 13940.0, 14060.0] {
        let p = peak_in_range(hz);
        if p > max_imd { max_imd = p; }
    }
    max_imd - fund_db
}

/// Build the test input for SMPTE IMD: 60 Hz at amplitude 0.4 plus
/// 7 kHz at amplitude 0.1 (4:1 ratio, total peak ≤ 0.5 to leave room).
pub fn imd_smpte_input(n: usize, sr: f32) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let t = i as f32 / sr;
            0.4 * (core::f32::consts::TAU * 60.0 * t).sin()
                + 0.1 * (core::f32::consts::TAU * 7000.0 * t).sin()
        })
        .collect()
}

/// Build a sine that lands cleanly on an FFT bin (no leakage). Useful as
/// the input to THD/aliasing tests. Returns `(samples, exact_freq_hz)`.
pub fn make_bin_aligned_sine(fft_len: usize, sr: f32, target_hz: f32, amplitude: f32) -> (Vec<f32>, f32) {
    let bin = (target_hz * fft_len as f32 / sr).round() as usize;
    let exact_freq = bin as f32 * sr / fft_len as f32;
    let samples: Vec<f32> = (0..fft_len)
        .map(|i| amplitude * (core::f32::consts::TAU * exact_freq * i as f32 / sr).sin())
        .collect();
    (samples, exact_freq)
}

// ---------------------------------------------------------------------------
// Wavetable band-limiting — drops every spectral bin above `max_harmonic`
// then transforms back to time domain. Used by wavetable synths to build
// the per-pitch mip pyramid that keeps high notes alias-free.
//
// Cost: 1 forward FFT + 1 inverse FFT of `samples.len()` (power-of-two
// preferred). NOT real-time safe — call once per preset / curve edit on
// the main thread.
// ---------------------------------------------------------------------------

pub fn lowpass_to_harmonics(samples: &[f32], max_harmonic: usize) -> Vec<f32> {
    let n = samples.len();
    let mut input = samples.to_vec();
    let mut planner = RealFftPlanner::<f32>::new();
    let fwd = planner.plan_fft_forward(n);
    let inv = planner.plan_fft_inverse(n);
    let mut spectrum = fwd.make_output_vec();
    fwd.process(&mut input, &mut spectrum).unwrap();
    // Bin 0 = DC, bins 1..=N/2 = harmonics 1..N/2. Drop anything past
    // max_harmonic (also drop the Nyquist bin if it would survive — that
    // ones halfband-aliased).
    let cut = (max_harmonic + 1).min(spectrum.len());
    for slot in spectrum.iter_mut().skip(cut) {
        *slot = Complex::new(0.0, 0.0);
    }
    let mut output = vec![0.0_f32; n];
    inv.process(&mut spectrum, &mut output).unwrap();
    // realfft's inverse leaves the result un-normalised; divide by N to
    // recover the original amplitude.
    let scale = 1.0 / n as f32;
    for v in output.iter_mut() {
        *v *= scale;
    }
    output
}

/// Compare a measured frequency response against a theoretical function.
/// Returns the maximum absolute deviation in dB across all measured freqs.
pub fn frequency_response_error_db<F>(
    measured: &[(f32, f32)],
    theoretical: F,
) -> f32
where
    F: Fn(f32) -> f32,
{
    let mut max_err = 0.0_f32;
    for &(f, m_db) in measured {
        let t_db = theoretical(f);
        let err = (m_db - t_db).abs();
        if err > max_err {
            max_err = err;
        }
    }
    max_err
}

#[cfg(test)]
mod measurement_tests {
    //! Unit tests for the measurement helpers themselves — feed known
    //! signals, check that the metrics return sensible numbers. Without
    //! these we'd be writing per-plugin assertions against potentially
    //! buggy measurement code.
    use super::*;

    const SR: f32 = 48_000.0;
    const FFT_LEN: usize = 16384;

    #[test]
    fn pure_sine_has_floor_thd() {
        // A clean sine should measure THD well below -60 dB (windowing
        // and numerical noise floor put us around -70 dB).
        let (samples, freq) = make_bin_aligned_sine(FFT_LEN, SR, 1000.0, 0.5);
        let thd = measure_thd_db(&samples, freq, SR);
        println!("pure sine THD: {thd:.1} dB");
        assert!(thd < -55.0, "pure sine should have THD < -55 dB, got {thd}");
    }

    #[test]
    fn clipped_sine_has_high_thd() {
        // Hard-clip a sine — should produce strong harmonic content.
        let (samples, freq) = make_bin_aligned_sine(FFT_LEN, SR, 1000.0, 0.9);
        let clipped: Vec<f32> = samples.iter().map(|&x| (x * 5.0).clamp(-0.5, 0.5)).collect();
        let thd = measure_thd_db(&clipped, freq, SR);
        println!("hard-clipped sine THD: {thd:.1} dB");
        assert!(thd > -25.0, "hard clip should have THD > -25 dB, got {thd}");
    }

    #[test]
    fn pure_sine_has_no_aliasing() {
        // A pure sine should have nothing in the spectrum but the
        // fundamental (and trace noise). measure_aliasing_db should
        // return a very negative number.
        let (samples, freq) = make_bin_aligned_sine(FFT_LEN, SR, 18_000.0, 0.5);
        let alias = measure_aliasing_db(&samples, freq, SR);
        println!("pure sine aliasing: {alias:.1} dB");
        assert!(alias < -55.0, "pure sine: aliasing should be < -55 dB, got {alias}");
    }

    #[test]
    fn aliased_sine_is_detected() {
        // Feed a sine that, after a non-linear stage (cubic distortion
        // here), produces a 3rd harmonic above Nyquist that aliases.
        // f0 = 18 kHz at 48 kHz → 3*f0 = 54 kHz mirrors to 6 kHz.
        // measure_aliasing_db should find that 6 kHz peak loud.
        let (samples, freq) = make_bin_aligned_sine(FFT_LEN, SR, 18_000.0, 0.5);
        let cubed: Vec<f32> = samples.iter().map(|&x| (x * 3.0).tanh()).collect();
        let alias = measure_aliasing_db(&cubed, freq, SR);
        println!("aliased sine: {alias:.1} dB");
        assert!(alias > -45.0, "should detect strong aliasing, got {alias}");
    }

    #[test]
    fn imd_baseline_is_low() {
        // Linear sum of two tones — no IMD products in the spectrum.
        let samples = imd_smpte_input(FFT_LEN, SR);
        let imd = measure_imd_smpte_db(&samples, SR);
        println!("baseline IMD (no nonlinearity): {imd:.1} dB");
        assert!(imd < -55.0, "linear sum has no IMD, got {imd}");
    }

    #[test]
    fn imd_under_nonlinearity_rises() {
        // Same SMPTE signal through tanh creates sum/diff products.
        let input = imd_smpte_input(FFT_LEN, SR);
        let processed: Vec<f32> = input.iter().map(|&x| (x * 4.0).tanh()).collect();
        let imd = measure_imd_smpte_db(&processed, SR);
        println!("non-linear IMD: {imd:.1} dB");
        assert!(imd > -45.0, "tanh should create measurable IMD, got {imd}");
    }

    #[test]
    fn frequency_response_error_zero_for_perfect_match() {
        let measured = vec![(100.0_f32, 0.0), (1000.0, 0.0), (10000.0, 0.0)];
        let err = frequency_response_error_db(&measured, |_| 0.0);
        assert!(err < 1e-6, "perfect match should be 0 dB error, got {err}");
    }

    #[test]
    fn frequency_response_error_picks_max() {
        let measured = vec![(100.0_f32, 0.5), (1000.0, 2.0), (10000.0, -1.0)];
        let err = frequency_response_error_db(&measured, |_| 0.0);
        assert!((err - 2.0).abs() < 1e-6, "expected max 2.0 dB, got {err}");
    }
}
