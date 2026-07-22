//! Regression test for the carrier-pitch "squeak" bug.
//!
//! Reported from live play: on a fast, plucky jaw-harp (kubyz) — a low
//! fundamental with strong upper harmonics and a hard transient on every
//! strike — the internal carrier's YIN tracker threw octave errors, so the
//! carrier jumped up into the mid/high register and squeaked on each attack
//! instead of sitting on the bass fundamental.
//!
//! This test synthesises exactly that scenario, runs it through the vocoder in
//! voice-tracking mode, and asserts the carrier pitch stays in the bass and
//! does not jump octaves. On the pre-fix build the output pitch median was
//! ~558 Hz (fundamental was ~73 Hz) with ~15 % of frames jumping >½ octave —
//! both assertions below fail there. With the median-window + portamento glide
//! + corrected tracker range, it locks to the fundamental.

use superduper_vocoder::dsp::{
    VocParams, Vocoder, MAX_VOICES, MODE_CLASSIC, PITCH_VOICE, SRC_INTERNAL, WAVE_SAW,
};

const SR: f32 = 48_000.0;
const F0: f32 = 73.0; // jaw-harp fundamental (D2-ish)

/// A plucky, harmonic-rich drone: weak fundamental, strong 3rd–6th harmonics
/// (what fools YIN into an octave error), plus a hard noise transient every
/// ~140 ms (fast strike rhythm).
fn plucky_kubyz(secs: f32) -> Vec<f32> {
    let n = (SR * secs) as usize;
    let mut rng: u32 = 0x1234_5678;
    let mut out = vec![0.0f32; n];
    for (i, s) in out.iter_mut().enumerate() {
        let t = i as f32 / SR;
        let mut v = 0.0f32;
        // Deliberately weak fundamental, loud mid harmonics (jaw-harp timbre).
        for h in 1..=24u32 {
            let amp = if h <= 2 { 0.25 } else { 1.0 / (h as f32).powf(0.5) };
            v += amp * (std::f32::consts::TAU * F0 * h as f32 * t).sin();
        }
        *s = v * 0.1;
    }
    // Hard transient plucks every ~140 ms.
    let period = (SR * 0.14) as usize;
    let mut p = 0usize;
    while p < n {
        let d = (SR * 0.012) as usize;
        for k in 0..d.min(n - p) {
            rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (rng >> 9) as f32 / (1u32 << 23) as f32 - 1.0;
            let env = 1.0 - k as f32 / d as f32;
            out[p + k] += 0.5 * noise * env;
        }
        p += period;
    }
    out
}

fn params() -> VocParams {
    VocParams {
        attack_ms: 5.0,
        release_ms: 55.0,
        source: SRC_INTERNAL,
        wave: WAVE_SAW,
        band_count: 16,
        pitch_source: PITCH_VOICE, // the mode with the bug
        notes: [-1i16; MAX_VOICES],
        pitch_offset_semi: 0.0,
        detune_cents: 0.0,
        formant_semi: 0.0,
        unvoiced: 0.0,
        drive: 0.0,
        mix: 1.0,
        output_lin: 1.0,
        mode: MODE_CLASSIC,
        detail: 1,
        bypassed: false,
    }
}

/// Autocorrelation pitch on one window, or None if too quiet.
fn window_hz(seg: &[f32]) -> Option<f32> {
    let n = seg.len();
    let mean = seg.iter().sum::<f32>() / n as f32;
    let rms = (seg.iter().map(|x| (x - mean) * (x - mean)).sum::<f32>() / n as f32).sqrt();
    if rms < 1e-3 {
        return None;
    }
    let lo = (SR / 800.0) as usize;
    let hi = ((SR / 40.0) as usize).min(n - 1);
    let (mut best_lag, mut best) = (lo, f32::MIN);
    for lag in lo..hi {
        let mut c = 0.0f32;
        for k in 0..n - lag {
            c += (seg[k] - mean) * (seg[k + lag] - mean);
        }
        if c > best {
            best = c;
            best_lag = lag;
        }
    }
    Some(SR / best_lag as f32)
}

/// Median of a slice (copy + sort, test-only).
fn median(v: &[f32]) -> f32 {
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    s[s.len() / 2]
}

#[test]
fn carrier_pitch_stays_on_the_bass_fundamental() {
    let modu = plucky_kubyz(2.5);
    let n = modu.len();
    let mut ol = vec![0.0f32; n];
    let mut or = vec![0.0f32; n];
    let sc = vec![0.0f32; n];
    let mut voc = Vocoder::new(SR);
    voc.process_stereo(&modu, &modu, &mut ol, &mut or, &sc, &sc, &params());

    // Trajectory of the output (carrier) pitch, skipping the first 0.3 s so the
    // tracker has locked.
    let win = 2048;
    let hop = 1024;
    let skip = (SR * 0.3) as usize;
    let mut traj = Vec::new();
    let mut i = skip;
    while i + win < n {
        if let Some(hz) = window_hz(&ol[i..i + win]) {
            traj.push(hz);
        }
        i += hop;
    }
    assert!(traj.len() > 10, "not enough voiced frames to judge");

    let med = median(&traj);
    let (lo, hi) = (
        traj.iter().cloned().fold(f32::MAX, f32::min),
        traj.iter().cloned().fold(f32::MIN, f32::max),
    );

    // Count large frame-to-frame octave jumps (the audible "squeaks").
    let jumps = traj
        .windows(2)
        .filter(|w| (w[1] / w[0]).log2().abs() > 0.5)
        .count();
    let jump_frac = jumps as f32 / (traj.len() - 1) as f32;

    println!(
        "carrier pitch: median={med:.0} Hz  range={lo:.0}..{hi:.0} Hz  \
         octave-jumps={pct:.1}% ({jumps}/{total})",
        pct = jump_frac * 100.0,
        total = traj.len() - 1
    );

    // The carrier must sit in the bass near the ~73 Hz fundamental, NOT jump to
    // the mid/high register. Pre-fix this was ~558 Hz.
    assert!(
        med < 220.0,
        "carrier pitch median {med:.0} Hz is way above the {F0:.0} Hz \
         fundamental — YIN is octave-erroring into the mids (the reported bug)"
    );
    // And it must be steady, not squeaking octaves on every pluck. Pre-fix ~15 %.
    assert!(
        jump_frac < 0.06,
        "carrier pitch jumps >½ octave on {:.0}% of frames — \
         unstable tracking (the reported squeak)",
        jump_frac * 100.0
    );
}
