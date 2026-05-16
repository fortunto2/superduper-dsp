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
