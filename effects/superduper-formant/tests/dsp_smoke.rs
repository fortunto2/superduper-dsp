//! DSP-level tests for SuperDuper Formant — no CLAP, no host.
//!
//! These check the things the plugin actually promises: that the resonators
//! genuinely carve formant peaks, that Follow mode copies a sung vowel onto the
//! main input, and that the vowel *stays* when the voice stops (the hand-off
//! that makes "voice → kubyz" sound continuous).
//!
//! Run: `cargo test --release -p superduper-formant --test dsp_smoke -- --nocapture`

use superduper_formant::dsp::{FmtParams, FormantFx, MODE_FOLLOW, MODE_MANUAL, MODE_MOTION};
use superduper_synth_core::analysis::spectrum_with_freq;
use superduper_synth_core::formant::{Formant, FORMANT_PRESETS};

const SR: f32 = 48_000.0;

/// Band-limited pulse train — a stand-in for both a glottal source and a kubyz
/// drone (dense harmonics are what formant filtering needs to bite on).
fn pulse_train(f0: f32, n: usize, amp: f32) -> Vec<f32> {
    let kmax = ((SR * 0.45).min(5_000.0) / f0) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let mut s = 0.0;
            for k in 1..=kmax {
                s += (std::f32::consts::TAU * f0 * k as f32 * t).sin() / k as f32;
            }
            s * amp
        })
        .collect()
}

/// Run a mono signal (plus optional sidechain) through the effect.
fn run(fx: &mut FormantFx, input: &[f32], sidechain: &[f32], p: &FmtParams) -> Vec<f32> {
    let mut out_l = vec![0.0f32; input.len()];
    let mut out_r = vec![0.0f32; input.len()];
    let sc: Vec<f32> = if sidechain.is_empty() {
        vec![0.0; input.len()]
    } else {
        sidechain.to_vec()
    };
    // Block-wise, like a host would.
    const BLOCK: usize = 512;
    let mut pos = 0;
    while pos < input.len() {
        let end = (pos + BLOCK).min(input.len());
        let (wl, wr) = (&mut out_l[pos..end], &mut out_r[pos..end]);
        fx.process_stereo(
            &input[pos..end],
            &input[pos..end],
            wl,
            wr,
            &sc[pos..end],
            &sc[pos..end],
            p,
        );
        pos = end;
    }
    out_l
}

/// Peak magnitude (dB) within `±tol_hz` of `hz`.
fn mag_near(spec: &[(f32, f32)], hz: f32, tol_hz: f32) -> f32 {
    spec.iter()
        .filter(|(f, _)| (*f - hz).abs() <= tol_hz)
        .map(|(_, m)| *m)
        .fold(f32::NEG_INFINITY, f32::max)
}

#[test]
fn silence_stays_silent_and_finite() {
    let mut fx = FormantFx::new(SR);
    let p = FmtParams::default();
    let out = run(&mut fx, &vec![0.0; 4800], &[], &p);
    assert!(out.iter().all(|v| v.is_finite()), "silence produced NaN/Inf");
    assert!(
        out.iter().all(|v| v.abs() < 1e-6),
        "silence produced output (self-oscillating resonators?)"
    );
}

#[test]
fn mix_zero_and_bypass_pass_dry_through() {
    let src = pulse_train(120.0, 4800, 0.3);
    for (label, p) in [
        ("mix=0", FmtParams { mix: 0.0, ..FmtParams::default() }),
        ("bypass", FmtParams { bypassed: true, ..FmtParams::default() }),
    ] {
        let mut fx = FormantFx::new(SR);
        let out = run(&mut fx, &src, &[], &p);
        let max_dev = src
            .iter()
            .zip(out.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_dev < 1e-6, "{label} must pass the dry signal (max dev {max_dev})");
    }
}

/// The core claim: three band-passes carve three peaks where the params say.
#[test]
fn resonators_carve_peaks_at_the_set_formants() {
    let vowel = FORMANT_PRESETS[1]; // Vowel A — 730 / 1090 / 2440
    let src = pulse_train(120.0, SR as usize / 2, 0.3);
    let mut fx = FormantFx::new(SR);
    let p = FmtParams {
        f1: vowel.f[0],
        f2: vowel.f[1],
        f3: vowel.f[2],
        mode: MODE_MANUAL,
        width: 1.0,
        mix: 1.0,
        ..FmtParams::default()
    };
    let out = run(&mut fx, &src, &[], &p);
    // Analyse a settled chunk.
    let spec = spectrum_with_freq(&out[out.len() - 8192..], SR);
    let f1 = mag_near(&spec, vowel.f[0], 90.0);
    let f2 = mag_near(&spec, vowel.f[1], 90.0);
    // Valley between F2 and F3 — nothing should live at 1650 Hz on an /ɑ/.
    let valley = mag_near(&spec, 1650.0, 90.0);
    eprintln!("A: F1 {f1:.1} dB  F2 {f2:.1} dB  valley@1650 {valley:.1} dB");
    assert!(
        f1 - valley > 10.0,
        "F1 peak must stand at least 10 dB above the inter-formant valley (got {:.1} dB)",
        f1 - valley
    );
    assert!(
        f2 - valley > 6.0,
        "F2 peak must stand above the valley (got {:.1} dB)",
        f2 - valley
    );
}

/// Follow mode: sing /i/ into the sidechain over a drone on the main input, and
/// the drone must take on /i/'s formants — not the pad's default 700/1200/2600.
#[test]
fn follow_copies_the_sung_vowel_onto_the_drone() {
    let vowel = FORMANT_PRESETS[3]; // /i/ — 270 / 2290 / 3010
    let n = SR as usize; // 1 s
    let drone = pulse_train(100.0, n, 0.3);
    // The "voice": a different f0 shaped by the vowel filter.
    let raw_voice = pulse_train(150.0, n, 0.3);
    let mut vfilt = Formant::default();
    let voice: Vec<f32> = raw_voice
        .iter()
        .map(|&s| vfilt.process(s, s, SR, vowel.f, vowel.bw, vowel.gain, 1.0).0)
        .collect();

    let mut fx = FormantFx::new(SR);
    let p = FmtParams {
        mode: MODE_FOLLOW,
        follow: 1.0,
        glide_ms: 20.0,
        mix: 1.0,
        ..FmtParams::default()
    };
    let out = run(&mut fx, &drone, &voice, &p);
    let tracked = fx.tracked_formants();
    let used = fx.current_formants();
    eprintln!("follow: tracked {tracked:?}  used {used:?}  (want ≈ {:?})", vowel.f);

    assert!(fx.tracker_active(), "a 1 s sung vowel must keep the tracker gate open");
    for i in 0..2 {
        let err = (tracked[i] - vowel.f[i]).abs() / vowel.f[i];
        assert!(
            err < 0.2,
            "tracked F{} off by {:.0}% (got {:.0} Hz, want {:.0} Hz)",
            i + 1,
            err * 100.0,
            tracked[i],
            vowel.f[i]
        );
    }
    // And the filter really used them (Follow = 1 → no pad influence left).
    for i in 0..3 {
        assert!(
            (used[i] - tracked[i]).abs() < tracked[i] * 0.1,
            "Follow=1 must filter at the tracked formants: used {:?} vs tracked {:?}",
            used,
            tracked
        );
    }
    // The output must actually be shaped: /i/ has a deep valley around 1 kHz.
    let spec = spectrum_with_freq(&out[out.len() - 8192..], SR);
    let f2 = mag_near(&spec, vowel.f[1], 120.0);
    let valley = mag_near(&spec, 1000.0, 100.0);
    assert!(
        f2 - valley > 6.0,
        "output should carry /i/'s bright F2 above the 1 kHz valley (Δ {:.1} dB)",
        f2 - valley
    );
}

/// When the singing stops the vowel must hold, not collapse — this freeze is
/// what lets a sung phrase hand over to the instrument.
#[test]
fn vowel_holds_after_the_voice_stops() {
    let vowel = FORMANT_PRESETS[3]; // /i/
    let n = SR as usize / 2;
    let drone = pulse_train(100.0, n * 2, 0.3);
    let raw_voice = pulse_train(150.0, n, 0.3);
    let mut vfilt = Formant::default();
    let mut voice: Vec<f32> = raw_voice
        .iter()
        .map(|&s| vfilt.process(s, s, SR, vowel.f, vowel.bw, vowel.gain, 1.0).0)
        .collect();
    voice.extend(std::iter::repeat(0.0).take(n)); // …then silence

    let mut fx = FormantFx::new(SR);
    let p = FmtParams {
        mode: MODE_FOLLOW,
        follow: 1.0,
        glide_ms: 20.0,
        ..FmtParams::default()
    };
    let _ = run(&mut fx, &drone, &voice, &p);
    let held = fx.current_formants();
    eprintln!("after silence: held {held:?} (sung {:?})", vowel.f);
    assert!(!fx.tracker_active(), "silence must close the tracker gate");
    for i in 0..2 {
        let err = (held[i] - vowel.f[i]).abs() / vowel.f[i];
        assert!(
            err < 0.25,
            "F{} must still hold the sung vowel after the voice stopped \
             (held {:.0} Hz vs sung {:.0} Hz)",
            i + 1,
            held[i],
            vowel.f[i]
        );
    }
}

/// Motion mode must actually move the formants, and Stereo must decorrelate the
/// channels (anti-phase trajectory).
#[test]
fn motion_moves_and_stereo_decorrelates() {
    let src = pulse_train(120.0, SR as usize, 0.3);
    let mut fx = FormantFx::new(SR);
    let p = FmtParams {
        mode: MODE_MOTION,
        path: 0, // Circle
        rate_hz: 2.0,
        depth: 1.0,
        ..FmtParams::default()
    };
    // Sample the used F1 across the run to see it travel.
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    let mut out_l = vec![0.0f32; 512];
    let mut out_r = vec![0.0f32; 512];
    let sc = vec![0.0f32; 512];
    let mut pos = 0;
    while pos + 512 <= src.len() {
        fx.process_stereo(
            &src[pos..pos + 512],
            &src[pos..pos + 512],
            &mut out_l,
            &mut out_r,
            &sc,
            &sc,
            &p,
        );
        let f = fx.current_formants()[0];
        lo = lo.min(f);
        hi = hi.max(f);
        pos += 512;
    }
    eprintln!("motion F1 travel: {lo:.0} … {hi:.0} Hz");
    assert!(
        hi - lo > 200.0,
        "Circle at Depth 1 should sweep F1 by ≳2·220 Hz, saw only {:.0} Hz",
        hi - lo
    );

    // Stereo = 1 → L/R run anti-phase, so the two channels must differ.
    let mut fx2 = FormantFx::new(SR);
    let p2 = FmtParams { stereo: 1.0, ..p };
    let mut l = vec![0.0f32; src.len()];
    let mut r = vec![0.0f32; src.len()];
    fx2.process_stereo(&src, &src, &mut l, &mut r, &vec![0.0; src.len()], &vec![0.0; src.len()], &p2);
    let diff: f32 = l
        .iter()
        .zip(r.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(diff > 1e-3, "Stereo=1 must decorrelate the channels (max |L−R| = {diff})");
}

/// Regression: the GUI used to call `presets::apply`, which writes the preset's
/// own `Preset` slot (table default = 0), so recall detection reverted every
/// pick to Default on the next block.
#[test]
fn preset_recall_leaves_the_preset_param_consistent() {
    use superduper_dsp_sdk::clap_helpers::preset_recall_target;
    use superduper_formant::{apply_preset_idx, PluginShared, P_PRESET};

    let shared = PluginShared::new();
    apply_preset_idx(&shared.inner, 7); // "Voice → Kubyz"
    let stored = shared.params[P_PRESET].load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(stored.round() as usize, 7, "P_PRESET must name the recalled preset");
    assert!(
        preset_recall_target(stored, &shared.active_preset,
    superduper_formant::presets::PRESETS.len(),
).is_none(),
        "recall must be quiet after applying — otherwise the next block reverts the pick"
    );
}
