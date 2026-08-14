//! DSP tests for SuperDuper Granular.
//!
//! The claims worth proving: the cloud actually sounds, Hann grains never click,
//! Freeze keeps producing audio after the input goes silent (the reason the
//! plugin exists), pitch shifts really transpose, and Density is level-
//! compensated so turning it up doesn't just get louder.
//!
//! Run: `cargo test --release -p superduper-granular --test dsp_smoke -- --nocapture`

use superduper_synth_core::analysis::spectrum_with_freq;
use superduper_synth_core::granular::{GrainParams, GranularCloud, SHAPE_HANN};

const SR: f32 = 48_000.0;

fn sine(hz: f32, n: usize, amp: f32) -> Vec<f32> {
    (0..n)
        .map(|i| (std::f32::consts::TAU * hz * i as f32 / SR).sin() * amp)
        .collect()
}

fn run(cloud: &mut GranularCloud, input: &[f32], p: &GrainParams) -> (Vec<f32>, Vec<f32>) {
    let mut l = vec![0.0f32; input.len()];
    let mut r = vec![0.0f32; input.len()];
    const BLOCK: usize = 512;
    let mut pos = 0;
    while pos < input.len() {
        let end = (pos + BLOCK).min(input.len());
        cloud.process(
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

fn max_step(x: &[f32]) -> f32 {
    x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
}

/// Spectral centroid in Hz — cheap proxy for "did the pitch move".
fn centroid(x: &[f32]) -> f32 {
    let spec = spectrum_with_freq(&x[x.len().saturating_sub(8192)..], SR);
    let mut num = 0.0;
    let mut den = 0.0;
    for (f, db) in spec {
        if f < 20.0 || f > 16_000.0 {
            continue;
        }
        let lin = 10f32.powf(db / 20.0);
        num += f * lin;
        den += lin;
    }
    if den > 0.0 {
        num / den
    } else {
        0.0
    }
}

#[test]
fn silence_stays_silent() {
    let mut c = GranularCloud::new(SR);
    let p = GrainParams::default();
    let (l, _) = run(&mut c, &vec![0.0; 24_000], &p);
    assert!(l.iter().all(|v| v.is_finite()), "NaN/Inf on silence");
    assert!(rms(&l) < 1e-6, "granulating silence produced sound");
}

#[test]
fn bypass_and_mix_zero_pass_dry() {
    let src = sine(220.0, 12_000, 0.4);
    for (label, p) in [
        ("bypass", GrainParams { bypassed: true, ..GrainParams::default() }),
        ("mix=0", GrainParams { mix: 0.0, ..GrainParams::default() }),
    ] {
        let mut c = GranularCloud::new(SR);
        let (l, _) = run(&mut c, &src, &p);
        let dev = src
            .iter()
            .zip(l.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(dev < 1e-6, "{label} must pass dry (dev {dev})");
    }
}

/// The cloud must sound, and Hann-windowed grains must not click — every grain
/// starts and ends at zero amplitude, so no discontinuity can be introduced.
#[test]
fn cloud_sounds_without_clicking() {
    let src = sine(220.0, SR as usize, 0.4);
    let mut c = GranularCloud::new(SR);
    let p = GrainParams {
        density: 60.0,
        size_ms: 50.0,
        spray: 0.4,
        shape: SHAPE_HANN,
        mix: 1.0,
        ..GrainParams::default()
    };
    let (l, r) = run(&mut c, &src, &p);
    let tail = &l[l.len() / 2..];
    eprintln!(
        "cloud: rms={:.4} max_step={:.4} (dry max_step={:.4}) grains={}",
        rms(tail),
        max_step(tail),
        max_step(&src),
        c.live_grains()
    );
    assert!(l.iter().all(|v| v.is_finite()), "NaN/Inf in output");
    assert!(rms(tail) > 1e-3, "cloud produced (near) silence — no grains sounding");
    assert!(
        max_step(tail) < 0.4,
        "sample-to-sample discontinuity {:.3} — grains are clicking",
        max_step(tail)
    );
    // Spread > 0 must actually decorrelate the channels.
    let diff = l
        .iter()
        .zip(r.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(diff > 1e-4, "Spread must produce a stereo difference (got {diff})");
}

/// Freeze: stop the input dead and the cloud must keep going. This is the
/// "sing one note → endless pad" behaviour, and the single most important
/// thing in this plugin.
#[test]
fn freeze_keeps_sounding_after_input_stops() {
    let mut c = GranularCloud::new(SR);
    let live = GrainParams {
        density: 40.0,
        size_ms: 150.0,
        spray: 0.3,
        mix: 1.0,
        ..GrainParams::default()
    };
    // 1 s of tone captured live.
    let tone = sine(330.0, SR as usize, 0.4);
    let _ = run(&mut c, &tone, &live);

    // Now: input silent, Freeze on.
    let frozen = GrainParams { freeze: true, ..live };
    let (l, _) = run(&mut c, &vec![0.0; SR as usize], &frozen);
    let level = rms(&l[l.len() / 2..]);
    eprintln!("frozen tail rms = {level:.4}");
    assert!(
        level > 1e-3,
        "frozen cloud went silent with no input (rms {level}) — capture buffer not being reused"
    );

    // Sanity: WITHOUT freeze the same silence must decay to nothing, otherwise
    // the test above proves nothing. Note the silence has to outlast the whole
    // 6 s capture buffer — after only 2 s the cloud is still legitimately
    // reading the tone that's still sitting in the un-overwritten tail.
    let mut c2 = GranularCloud::new(SR);
    let _ = run(&mut c2, &tone, &live);
    let (l2, _) = run(&mut c2, &vec![0.0; (SR * 8.0) as usize], &live);
    let tail2 = rms(&l2[l2.len() - SR as usize / 2..]);
    eprintln!("unfrozen tail rms = {tail2:.6}");
    assert!(
        tail2 < level * 0.2,
        "unfrozen cloud should fade as the buffer fills with silence (got {tail2} vs frozen {level})"
    );
}

#[test]
fn pitch_transposes_the_grains() {
    let src = sine(220.0, SR as usize, 0.4);
    let base = GrainParams {
        density: 40.0,
        size_ms: 120.0,
        spray: 0.1,
        mix: 1.0,
        ..GrainParams::default()
    };
    let mut c0 = GranularCloud::new(SR);
    let (flat, _) = run(&mut c0, &src, &base);
    let mut c_up = GranularCloud::new(SR);
    let (up, _) = run(&mut c_up, &src, &GrainParams { pitch_semi: 12.0, ..base });
    let mut c_dn = GranularCloud::new(SR);
    let (down, _) = run(&mut c_dn, &src, &GrainParams { pitch_semi: -12.0, ..base });

    let (c_flat, c_up_hz, c_dn_hz) = (centroid(&flat), centroid(&up), centroid(&down));
    eprintln!("centroid: −12 {c_dn_hz:.0} Hz | 0 {c_flat:.0} Hz | +12 {c_up_hz:.0} Hz");
    assert!(
        c_up_hz > c_flat * 1.4,
        "+12 st must raise the centroid ({c_flat:.0} → {c_up_hz:.0} Hz)"
    );
    assert!(
        c_dn_hz < c_flat * 0.8,
        "−12 st must lower the centroid ({c_flat:.0} → {c_dn_hz:.0} Hz)"
    );
}

/// Density is level-compensated by √overlap: raising it should thicken the
/// texture, not just make it louder.
#[test]
fn density_is_level_compensated() {
    let src = sine(220.0, SR as usize, 0.4);
    let base = GrainParams {
        size_ms: 100.0,
        spray: 0.3,
        mix: 1.0,
        ..GrainParams::default()
    };
    let mut c_lo = GranularCloud::new(SR);
    let (lo, _) = run(&mut c_lo, &src, &GrainParams { density: 20.0, ..base });
    let mut c_hi = GranularCloud::new(SR);
    let (hi, _) = run(&mut c_hi, &src, &GrainParams { density: 120.0, ..base });
    let (r_lo, r_hi) = (rms(&lo[lo.len() / 2..]), rms(&hi[hi.len() / 2..]));
    let ratio_db = 20.0 * (r_hi / r_lo.max(1e-9)).log10();
    eprintln!("density 20 → {r_lo:.4}, 120 → {r_hi:.4}  ({ratio_db:+.1} dB)");
    assert!(
        ratio_db.abs() < 9.0,
        "6× density should stay within ~9 dB after compensation, got {ratio_db:+.1} dB"
    );
}

/// Regression: recalling "Freeze Pad" on a fresh instance ground an all-zero
/// ring forever, because Freeze skipped the capture before anything was in it.
#[test]
fn freeze_from_the_first_sample_still_produces_audio() {
    let mut c = GranularCloud::new(SR);
    let p = GrainParams {
        freeze: true,
        density: 40.0,
        size_ms: 150.0,
        spray: 0.3,
        mix: 1.0,
        ..GrainParams::default()
    };
    let tone = sine(330.0, (SR * 2.0) as usize, 0.4);
    let (l, _) = run(&mut c, &tone, &p);
    let level = rms(&l[l.len() / 2..]);
    eprintln!("freeze-from-boot: rms {level:.4}");
    assert!(
        level > 1e-3,
        "Freeze on a fresh instance must fall back to capturing, not grind silence (rms {level})"
    );
}

/// Regression: the start-position guard was a flat 0.2 % of the ring, so reverse
/// grains and pitched-up grains read across the write head into 6-second-old
/// audio and spliced a discontinuity mid-grain.
#[test]
fn reverse_and_pitched_grains_do_not_splice_across_the_write_head() {
    let src = sine(220.0, (SR * 2.0) as usize, 0.4);
    for (label, p) in [
        (
            "reverse",
            GrainParams {
                reverse: 1.0,
                density: 30.0,
                size_ms: 80.0,
                spray: 0.0,
                position: 0.0,
                mix: 1.0,
                ..GrainParams::default()
            },
        ),
        (
            "pitch +12",
            GrainParams {
                pitch_semi: 12.0,
                density: 30.0,
                size_ms: 80.0,
                spray: 0.0,
                position: 0.0,
                mix: 1.0,
                ..GrainParams::default()
            },
        ),
    ] {
        let mut c = GranularCloud::new(SR);
        let (l, _) = run(&mut c, &src, &p);
        let tail = &l[l.len() / 2..];
        let step = max_step(tail);
        eprintln!("{label}: max_step {step:.4}");
        assert!(
            step < 0.4,
            "{label} grains splice across the write head — discontinuity {step:.3}"
        );
    }
}

/// Same preset-recall regression as the other plugins (see stretch's copy).
#[test]
fn preset_recall_leaves_the_preset_param_consistent() {
    use superduper_dsp_sdk::clap_helpers::preset_recall_target;
    use superduper_granular::{apply_preset_idx, PluginShared, P_PRESET};

    let shared = PluginShared::new();
    apply_preset_idx(&shared.inner, 1);
    let stored = shared.params[P_PRESET].load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(stored.round() as usize, 1);
    assert!(preset_recall_target(stored, &shared.active_preset).is_none());
}
