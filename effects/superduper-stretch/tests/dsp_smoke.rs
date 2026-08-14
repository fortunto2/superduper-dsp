//! DSP tests for SuperDuper Stretch.
//!
//! The claims worth proving: the read head really crawls at 1/Stretch, the
//! magnitude spectrum survives phase randomisation (a 440 Hz tone stays a 440 Hz
//! tone), the incoherent-OLA make-up keeps the level sane, Freeze sustains with
//! no input, and Pitch transposes.
//!
//! Run: `cargo test --release -p superduper-stretch --test dsp_smoke -- --nocapture`

use superduper_synth_core::analysis::spectrum_with_freq;
use superduper_synth_core::paulstretch::{PaulStretch, StretchParams, BUFFER_SECONDS};

const SR: f32 = 48_000.0;

fn sine(hz: f32, n: usize, amp: f32) -> Vec<f32> {
    (0..n)
        .map(|i| (std::f32::consts::TAU * hz * i as f32 / SR).sin() * amp)
        .collect()
}

fn run(fx: &mut PaulStretch, input: &[f32], p: &StretchParams) -> (Vec<f32>, Vec<f32>) {
    let mut l = vec![0.0f32; input.len()];
    let mut r = vec![0.0f32; input.len()];
    const BLOCK: usize = 512;
    let mut pos = 0;
    while pos < input.len() {
        let end = (pos + BLOCK).min(input.len());
        fx.process(
            &input[pos..end],
            &input[pos..end],
            &mut l[pos..end],
            &mut r[pos..end],
            p,
        );
        pos = end;
    }
    (l, r)
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// Frequency of the loudest spectral peak above 40 Hz.
fn peak_hz(x: &[f32]) -> f32 {
    let tail = &x[x.len().saturating_sub(16384)..];
    let spec = spectrum_with_freq(tail, SR);
    spec.iter()
        .filter(|(f, _)| *f > 40.0 && *f < 16_000.0)
        .fold((0.0f32, f32::NEG_INFINITY), |acc, &(f, db)| {
            if db > acc.1 {
                (f, db)
            } else {
                acc
            }
        })
        .0
}

#[test]
fn silence_stays_silent() {
    let mut fx = PaulStretch::new(SR);
    let p = StretchParams::default();
    let (l, _) = run(&mut fx, &vec![0.0; (SR * 2.0) as usize], &p);
    assert!(l.iter().all(|v| v.is_finite()), "NaN/Inf on silence");
    assert!(rms(&l) < 1e-6, "stretching silence produced sound");
}

#[test]
fn bypass_and_mix_zero_pass_dry() {
    let src = sine(220.0, 24_000, 0.4);
    for (label, p) in [
        ("bypass", StretchParams { bypassed: true, ..StretchParams::default() }),
        ("mix=0", StretchParams { mix: 0.0, ..StretchParams::default() }),
    ] {
        let mut fx = PaulStretch::new(SR);
        let (l, _) = run(&mut fx, &src, &p);
        let dev = src
            .iter()
            .zip(l.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(dev < 1e-6, "{label} must pass dry (dev {dev})");
    }
}

/// The core of the algorithm: the read head must advance at `1/Stretch` of real
/// time. Measured in Freeze so no catch-up jump can muddy the reading.
#[test]
fn read_head_crawls_at_one_over_stretch() {
    for stretch in [2.0f32, 10.0, 30.0] {
        let mut fx = PaulStretch::new(SR);
        let tone = sine(220.0, (SR * 1.5) as usize, 0.4);
        let live = StretchParams { stretch, window: 1, ..StretchParams::default() };
        let _ = run(&mut fx, &tone, &live);

        let frozen = StretchParams { freeze: true, length_s: 10.0, ..live };
        // One block to latch the freeze anchor, then measure over exactly 1 s.
        let _ = run(&mut fx, &vec![0.0; 512], &frozen);
        let before = fx.read_phase();
        let _ = run(&mut fx, &vec![0.0; SR as usize], &frozen);
        let after = fx.read_phase();

        let moved_frac = (after - before).rem_euclid(1.0);
        let moved_s = moved_frac * BUFFER_SECONDS;
        let want_s = 1.0 / stretch;
        eprintln!("stretch {stretch:>4.0}× → read head moved {moved_s:.4} s in 1 s (want {want_s:.4})");
        assert!(
            (moved_s - want_s).abs() < want_s * 0.25 + 0.01,
            "at {stretch}× the read head should advance ~{want_s:.3} s per second, moved {moved_s:.3}"
        );
    }
}

/// Randomising phase must not move energy in frequency: a 440 Hz tone stretched
/// 8× is still a 440 Hz tone (that's the whole reason the trick works).
#[test]
fn spectrum_survives_phase_randomisation() {
    let mut fx = PaulStretch::new(SR);
    let tone = sine(440.0, (SR * 3.0) as usize, 0.4);
    let p = StretchParams {
        stretch: 8.0,
        window: 2,
        tonal: 0.0,
        mix: 1.0,
        ..StretchParams::default()
    };
    let (l, _) = run(&mut fx, &tone, &p);
    let hz = peak_hz(&l);
    eprintln!("440 Hz stretched 8× → peak at {hz:.1} Hz, rms {:.4}", rms(&l[l.len() / 2..]));
    assert!(l.iter().all(|v| v.is_finite()), "NaN/Inf in output");
    assert!(
        (hz - 440.0).abs() < 25.0,
        "stretched tone should still peak at 440 Hz, got {hz:.1} Hz"
    );
}

/// Level sanity: the incoherent-OLA make-up should land the wet output within a
/// few dB of the input instead of √2 quiet (or loud).
#[test]
fn output_level_is_in_the_same_ballpark_as_input() {
    let tone = sine(220.0, (SR * 3.0) as usize, 0.4);
    let in_rms = rms(&tone);
    for tonal in [0.0f32, 1.0] {
        let mut fx = PaulStretch::new(SR);
        let p = StretchParams {
            stretch: 6.0,
            window: 2,
            tonal,
            mix: 1.0,
            ..StretchParams::default()
        };
        let (l, _) = run(&mut fx, &tone, &p);
        let out_rms = rms(&l[l.len() / 2..]);
        let db = 20.0 * (out_rms / in_rms.max(1e-9)).log10();
        eprintln!("tonal {tonal}: in {in_rms:.4} → out {out_rms:.4} ({db:+.1} dB)");
        assert!(
            db.abs() < 7.0,
            "tonal={tonal} output is {db:+.1} dB off the input — OLA normalisation is wrong"
        );
    }
}

/// Freeze must sustain with no input at all — the "sing one note, hold it
/// forever" behaviour, and it must stay tonal rather than decaying to noise.
#[test]
fn freeze_sustains_without_input() {
    let mut fx = PaulStretch::new(SR);
    let tone = sine(330.0, (SR * 2.0) as usize, 0.4);
    let live = StretchParams { stretch: 10.0, window: 2, ..StretchParams::default() };
    let _ = run(&mut fx, &tone, &live);

    let frozen = StretchParams { freeze: true, length_s: 1.5, ..live };
    let (l, _) = run(&mut fx, &vec![0.0; (SR * 4.0) as usize], &frozen);
    let tail = &l[l.len() / 2..];
    let level = rms(tail);
    let hz = peak_hz(tail);
    eprintln!("frozen 4 s with no input: rms {level:.4}, peak {hz:.1} Hz");
    assert!(level > 1e-3, "frozen output went silent (rms {level})");
    assert!(
        (hz - 330.0).abs() < 30.0,
        "frozen pad should still be the captured 330 Hz tone, got {hz:.1} Hz"
    );
}

#[test]
fn pitch_shifts_the_spectrum() {
    let tone = sine(220.0, (SR * 3.0) as usize, 0.4);
    let base = StretchParams {
        stretch: 8.0,
        window: 2,
        mix: 1.0,
        ..StretchParams::default()
    };
    let mut up = PaulStretch::new(SR);
    let (l_up, _) = run(&mut up, &tone, &StretchParams { pitch_semi: 12.0, ..base });
    let mut down = PaulStretch::new(SR);
    let (l_dn, _) = run(&mut down, &tone, &StretchParams { pitch_semi: -12.0, ..base });

    let (hz_up, hz_dn) = (peak_hz(&l_up), peak_hz(&l_dn));
    eprintln!("220 Hz: +12 st → {hz_up:.1} Hz, −12 st → {hz_dn:.1} Hz");
    assert!(
        (hz_up - 440.0).abs() < 40.0,
        "+12 st should land near 440 Hz, got {hz_up:.1}"
    );
    assert!(
        (hz_dn - 110.0).abs() < 25.0,
        "−12 st should land near 110 Hz, got {hz_dn:.1}"
    );
}

/// A window-size change mid-stream must not produce a discontinuity burst — the
/// accumulator is reset, so the worst case is a short gap, never a click.
#[test]
fn window_change_does_not_blow_up() {
    let mut fx = PaulStretch::new(SR);
    let tone = sine(220.0, (SR * 1.5) as usize, 0.4);
    let mut p = StretchParams { stretch: 6.0, window: 1, ..StretchParams::default() };
    let _ = run(&mut fx, &tone, &p);
    for w in [3usize, 0, 4, 2] {
        p.window = w;
        let (l, _) = run(&mut fx, &tone, &p);
        assert!(l.iter().all(|v| v.is_finite()), "NaN/Inf after window→{w}");
        let peak = l.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
        assert!(peak < 4.0, "window→{w} produced a {peak:.1} peak — accumulator not reset");
    }
}

/// Regression: recalling "Freeze Pad" on a fresh instance used to output dry
/// forever — gating the capture also gated the priming counter, so the plugin
/// never had a window to stretch and Freeze locked it onto an empty ring.
#[test]
fn freeze_from_the_first_sample_still_produces_audio() {
    let mut fx = PaulStretch::new(SR);
    let p = StretchParams {
        freeze: true,
        stretch: 8.0,
        window: 1,
        mix: 1.0,
        ..StretchParams::default()
    };
    let tone = sine(330.0, (SR * 3.0) as usize, 0.4);
    let (l, _) = run(&mut fx, &tone, &p);
    let level = rms(&l[l.len() / 2..]);
    let hz = peak_hz(&l);
    eprintln!("freeze-from-boot: rms {level:.4}, peak {hz:.1} Hz");
    assert!(
        level > 1e-3,
        "Freeze on a fresh instance must fall back to capturing, not loop silence (rms {level})"
    );
    assert!((hz - 330.0).abs() < 30.0, "should be stretching the captured tone, got {hz:.1} Hz");
}

/// Regression: leaving Freeze re-seated the read head half a ring (6 s) back,
/// which on a 2-second-old instance is pure silence — and at 8x stretch it takes
/// ~30 s of playback to crawl out of it.
#[test]
fn unfreezing_stays_inside_what_was_captured() {
    let mut fx = PaulStretch::new(SR);
    let live = StretchParams { stretch: 8.0, window: 1, mix: 1.0, ..StretchParams::default() };
    let tone = sine(220.0, (SR * 2.0) as usize, 0.4);
    let _ = run(&mut fx, &tone, &live);
    // Freeze briefly...
    let _ = run(&mut fx, &vec![0.0; 4096], &StretchParams { freeze: true, ..live });
    // ...then back to live with real input again.
    let (l, _) = run(&mut fx, &tone, &live);
    let level = rms(&l[l.len() / 2..]);
    eprintln!("after un-freeze: rms {level:.4}");
    assert!(
        level > 1e-3,
        "un-freezing must not seat the read head in the never-written part of the ring (rms {level})"
    );
}

/// Regression: the GUI used to call `presets::apply`, which writes the preset's
/// own `Preset` slot (table default = 0) and left recall detection thinking the
/// host wanted preset 0 — so every GUI pick instantly reverted to Default.
#[test]
fn preset_recall_leaves_the_preset_param_consistent() {
    use superduper_dsp_sdk::clap_helpers::preset_recall_target;
    use superduper_stretch::{apply_preset_idx, PluginShared, P_PRESET};

    let shared = PluginShared::new();
    apply_preset_idx(&shared.inner, 2);
    let stored = shared.params[P_PRESET].load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(stored.round() as usize, 2, "P_PRESET must name the recalled preset");
    assert!(
        preset_recall_target(stored, &shared.active_preset,
    superduper_stretch::presets::PRESETS.len(),
).is_none(),
        "recall must be quiet after applying — otherwise the next block reverts the pick"
    );
}
