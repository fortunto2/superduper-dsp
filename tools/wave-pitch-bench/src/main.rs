//! `wave-pitch-bench` — see how much wavetable detail survives the
//! mip-pyramid + interpolated read at different play pitches.
//!
//! Generates a handful of canonical wavetables (sine, triangle, saw,
//! square, custom edited curve), builds the mip pyramid the plugin
//! uses, then synthesises 1 s of audio at each test pitch (A2, A3,
//! A4, A5, A6, A7). FFTs the result and prints an ASCII spectrum so
//! you can read off:
//!
//! - which mip level was picked for each pitch
//! - how many harmonics actually made it through
//! - whether subtle wavetables (similar low-order content but
//!   different high-order detail) collapse to identical-looking
//!   spectra on high notes
//!
//! Run:
//!   cargo run --release -p wave-pitch-bench
//!
//! Output goes to stdout — one ASCII chart per (wavetable, pitch).

use superduper_synth_core::analysis::{ascii_spectrum, spectrum_with_freq, AsciiSpectrumOpts};
use superduper_wave::osc::{mip_from_table, MipWavetable, WT_SIZE};

const SR: f32 = 48_000.0;
const N_SECONDS: usize = 1;
const TEST_PITCHES_HZ: &[(&str, f32)] = &[
    ("A2  110 Hz", 110.0),
    ("A3  220 Hz", 220.0),
    ("A4  440 Hz", 440.0),
    ("A5  880 Hz", 880.0),
    ("A6  1760 Hz", 1760.0),
    ("A7  3520 Hz", 3520.0),
];

fn sine_table() -> Vec<f32> {
    (0..WT_SIZE)
        .map(|i| (2.0 * std::f32::consts::PI * i as f32 / WT_SIZE as f32).sin())
        .collect()
}

fn triangle_table() -> Vec<f32> {
    (0..WT_SIZE)
        .map(|i| {
            let p = i as f32 / WT_SIZE as f32;
            if p < 0.5 { 4.0 * p - 1.0 } else { 3.0 - 4.0 * p }
        })
        .collect()
}

/// Naive saw (full bandwidth — aliases on its own; pyramid bandlimits
/// it). This is the worst-case for the mip pyramid because the
/// sawtooth has every integer harmonic with 1/n amplitude.
fn saw_table() -> Vec<f32> {
    (0..WT_SIZE)
        .map(|i| {
            let p = i as f32 / WT_SIZE as f32;
            2.0 * p - 1.0
        })
        .collect()
}

fn square_table() -> Vec<f32> {
    (0..WT_SIZE)
        .map(|i| {
            let p = i as f32 / WT_SIZE as f32;
            if p < 0.5 { 1.0 } else { -1.0 }
        })
        .collect()
}

/// A "subtle" wavetable — sine + a tiny bit of 50th harmonic. The
/// gross shape looks like a sine in the editor, the high-frequency
/// detail is the whole point. Use this to test whether the mip
/// pyramid swallows subtle detail at moderate pitches.
fn subtle_high_detail_table() -> Vec<f32> {
    (0..WT_SIZE)
        .map(|i| {
            let p = i as f32 / WT_SIZE as f32;
            let twopi = 2.0 * std::f32::consts::PI;
            (twopi * p).sin() + 0.05 * (twopi * 50.0 * p).sin()
        })
        .collect()
}

/// Render `freq_hz` for `N_SECONDS` from `mip` and return the samples.
/// Read uses the same routine the live oscillator uses (single-frame).
fn render(mip: &MipWavetable, freq_hz: f32) -> Vec<f32> {
    use superduper_wave::osc::read_single_for_test;
    let n = (SR as usize) * N_SECONDS;
    let level = mip.pick_level(freq_hz, SR, true);
    let table = &*mip.levels[level];
    let mut phase = 0.0f32;
    let step = freq_hz / SR;
    (0..n)
        .map(|_| {
            let y = read_single_for_test(table, phase);
            phase = (phase + step).fract();
            y
        })
        .collect()
}

fn report_one(name: &str, base: Vec<f32>) {
    println!("\n══ {name} ══");
    let mip = mip_from_table(&base);
    for (label, freq) in TEST_PITCHES_HZ {
        let buf = render(&mip, *freq);
        // Take a steady-state window of 4096 samples for the FFT — skip
        // the first 1000 to avoid initial transient.
        let window = &buf[1000..1000 + 4096];
        let spec = spectrum_with_freq(window, SR);
        let chart = ascii_spectrum(
            &spec,
            &AsciiSpectrumOpts {
                rows: 8,
                cols: 70,
                min_db: -80.0,
                max_db: 0.0,
                min_hz: 20.0,
                max_hz: 20_000.0,
                log_freq: true,
            },
        );
        let level = mip.pick_level(*freq, SR, true);
        let max_harmonics = ((WT_SIZE / 2) >> level).max(1);
        println!(
            "── {label} → mip level {level} ({max_harmonics} harmonics allowed)",
        );
        for line in chart.lines() {
            println!("    {line}");
        }
    }
}

fn main() {
    println!(
        "wave-pitch-bench: sr={} Hz, WT_SIZE={WT_SIZE}, {} test pitches",
        SR as u32,
        TEST_PITCHES_HZ.len(),
    );
    report_one("Sine", sine_table());
    report_one("Triangle", triangle_table());
    report_one("Saw (naive)", saw_table());
    report_one("Square", square_table());
    report_one("Subtle (sine + 5% of 50th harmonic)", subtle_high_detail_table());
    println!("\nLegend:");
    println!("  bars are dB magnitude per FFT bin, linear-Hz, 4096-pt FFT");
    println!("  bin width = sr / 4096 = {:.1} Hz", SR / 4096.0);
}
