//! Reference signals for anything that has to answer "does this material suit
//! a pitch-synchronous engine?".
//!
//! These live here rather than in each test file because the numbers they
//! produce are quoted against each other: the sharpness table in
//! `synth_core::pitch::epoch_sharpness`'s doc pairs a value measured on
//! `pulsed()` with a dB value measured on the *same* `pulsed()` in
//! `superduper-pitch/tests/engine_transparency.rs`. Three private copies had
//! already drifted before anyone noticed — one of them silently dropped the
//! aspiration path from `voiced` while still calling itself a mirror.
//!
//! [`probes`](crate::probes) holds the generic measurements (rms, peak,
//! max_step, thd_at); this module holds the sources and the one measurement
//! that only makes sense for them, [`noise_to_harmonic`].

use superduper_synth_core::dsp_blocks::{Biquad, Xorshift};

/// The sample rate every generator here works at.
pub const SR: f32 = 48_000.0;
/// The fundamental every generator here sings at.
pub const F0: f32 = 220.0;

/// Smooth harmonic tone — a synth, a sustained kubyz, any sum-of-sines. No
/// glottal pulse anywhere in it, so a grain scheduler has nothing to snap to.
pub fn smooth(secs: f32) -> Vec<f32> {
    smooth_at(secs, F0)
}

/// Same, at an arbitrary fundamental.
pub fn smooth_at(secs: f32, f0: f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            (1..=6)
                .map(|k| (core::f32::consts::TAU * f0 * k as f32 * t).sin() / k as f32)
                .sum::<f32>()
                * 0.3
        })
        .collect()
}

/// Impulse train through two formant resonators — one sharp, unambiguous
/// epoch per period. The material TD-PSOLA was designed for.
pub fn pulsed(secs: f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    let period = SR / F0;
    let mut b1 = Biquad::default();
    b1.set_bandpass(SR, 700.0, 5.0);
    let mut b2 = Biquad::default();
    b2.set_bandpass(SR, 1800.0, 8.0);
    let mut ph = 0.0f32;
    (0..n)
        .map(|_| {
            ph += 1.0;
            let src = if ph >= period {
                ph -= period;
                1.0
            } else {
                0.0
            };
            (b1.process(src) + 0.5 * b2.process(src)) * 0.6
        })
        .collect()
}

/// A normally-phonated sung note: [`voiced_at`] at [`F0`] with a little
/// aspiration and a normal open quotient.
pub fn voiced(secs: f32) -> Vec<f32> {
    voiced_at(secs, F0, 0.02, 0.4)
}

/// A breathy take. Still unambiguously a voice, but the long open quotient
/// blunts the glottal closure and the aspiration fills the rest of the period,
/// which is what makes it behave like a smooth tone under PSOLA.
pub fn breathy(secs: f32) -> Vec<f32> {
    voiced_at(secs, F0, 0.5, 0.62)
}

/// Rosenberg glottal-flow pulses with jitter and shimmer through three
/// formants, plus `breath` of aspiration noise and an `open` quotient.
///
/// `f0` below 95 Hz is the case that never locked before the engine's floor
/// moved — a bass at 87 Hz.
pub fn voiced_at(secs: f32, f0: f32, breath: f32, open: f32) -> Vec<f32> {
    glottal(secs, breath, open, |_| f0)
}

/// The one glottal source. `f0_at(t)` is evaluated once per cycle, so a
/// constant closure gives a held note and a varying one gives a glide or a
/// vibrato — the period rule was the only thing that ever differed.
///
/// It is one function on purpose: this module exists because three private
/// copies of these generators drifted, and a fork in here would be the same
/// failure one level in. The constants below are the source of truth for
/// every voice in the suite.
fn glottal(secs: f32, breath: f32, open: f32, f0_at: impl Fn(f32) -> f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    let mut rng = Xorshift::new(0x5EED_1234);
    let mut bank: Vec<Biquad> = [(730.0, 9.0), (1090.0, 11.0), (2440.0, 13.0)]
        .iter()
        .map(|&(hz, q)| {
            let mut b = Biquad::default();
            b.set_bandpass(SR, hz, q);
            b
        })
        .collect();
    // Aspiration is band-limited, not flat white — real breath noise sits
    // above ~1 kHz where the tract is not damped by the glottis.
    let mut air = Biquad::default();
    air.set_bandpass(SR, 2600.0, 0.8);

    let mut ph = 0.0f32;
    let mut period = SR / f0_at(0.0);
    let mut amp = 1.0f32;
    let mut prev_g = 0.0f32;
    (0..n)
        .map(|i| {
            ph += 1.0;
            if ph >= period {
                ph -= period;
                // jitter ±0.4 %, shimmer ±6 % — enough to be a voice, not
                // enough to blur the harmonic grid the metric measures on.
                period = SR / f0_at(i as f32 / SR) * (1.0 + 0.004 * rng.next_bipolar());
                amp = 1.0 + 0.06 * rng.next_bipolar();
            }
            let t = ph / period;
            let (t1, t2) = (open, open * 0.4);
            let g = amp
                * if t < t1 {
                    0.5 * (1.0 - (core::f32::consts::PI * t / t1).cos())
                } else if t < t1 + t2 {
                    (core::f32::consts::FRAC_PI_2 * (t - t1) / t2).cos()
                } else {
                    0.0
                };
            // The excitation is the DERIVATIVE of glottal flow — the sharp
            // negative spike at closure IS the epoch a grain scheduler looks
            // for, and `open` is what softens it.
            let exc = (g - prev_g) * 40.0;
            prev_g = g;
            let noise = air.process(rng.next_bipolar()) * breath;
            let out: f32 = bank.iter_mut().map(|b| b.process(exc + noise)).sum();
            out * 0.5
        })
        .collect()
}

/// Noise energy (everything off the harmonic grid) over harmonic energy, in
/// dB. Lower is cleaner. `skip` seconds are stepped over first, so a caller
/// can start measuring past an engine's latency and settling time.
///
/// **Assumes one stationary f0.** On a sung phrase the pitch moves, every
/// harmonic lands off the fixed grid and the number goes positive and stops
/// meaning anything — a real lead-vocal take measures about +22 dB at its own
/// input. Use it on the generators above, or on a held note.
pub fn noise_to_harmonic(x: &[f32], f0: f32, skip: f32) -> f32 {
    let start = (skip * SR) as usize;
    let len = (1.0 * SR) as usize;
    let seg: Vec<f32> = x[start..start + len]
        .iter()
        .enumerate()
        .map(|(i, &v)| v * (0.5 - 0.5 * (core::f32::consts::TAU * i as f32 / len as f32).cos()))
        .collect();
    let spec = superduper_synth_core::analysis::magnitude_spectrum_db(&seg);
    let bin_hz = SR / len as f32;
    let (mut harm, mut noise) = (0.0f64, 0.0f64);
    for (i, &db) in spec.iter().enumerate() {
        let hz = i as f32 * bin_hz;
        if !(60.0..=8000.0).contains(&hz) {
            continue;
        }
        let lin = 10f64.powf(db as f64 / 20.0);
        let e = lin * lin;
        if (1..=36).any(|k| (hz - f0 * k as f32).abs() < 7.0 * bin_hz) {
            harm += e;
        } else {
            noise += e;
        }
    }
    10.0 * ((noise / harm.max(1e-30)) as f32).log10()
}

/// Drive `f` over `x` in `block`-sized chunks and collect the output.
///
/// Every DSP test in the workspace grew its own version of this loop, and the
/// copies had already diverged in three ways: some dropped the final partial
/// block, some allocated a fresh scratch buffer inside the loop, and the
/// per-block hook existed in only two of them. `at` is the block's start
/// frame, which is what a test needs to change a parameter mid-render.
pub fn render_blocks(
    x: &[f32],
    block: usize,
    mut f: impl FnMut(usize, &[f32], &mut [f32]),
) -> Vec<f32> {
    let mut out = vec![0.0; x.len()];
    let mut at = 0;
    while at < x.len() {
        let n = block.min(x.len() - at);
        f(at, &x[at..at + n], &mut out[at..at + n]);
        at += n;
    }
    out
}

/// Load a real recording, mono-summed and naively resampled to [`SR`], taking
/// the loudest `secs` so a leading silence does not dominate the window.
///
/// Returns `None` when the variable is unset or the file will not parse, so a
/// test that offers a real-material escape hatch stays hermetic by default.
pub fn take_from_env(var: &str, secs: f32) -> Option<Vec<f32>> {
    let path = std::env::var(var).ok()?;
    let w = superduper_synth_core::wav::parse_wav_file(std::path::Path::new(&path)).ok()?;
    let frames = w.frame_count();
    if frames == 0 {
        return None;
    }
    let ratio = w.sample_rate as f32 / SR;
    let n = (frames as f32 / ratio) as usize;
    let all: Vec<f32> = (0..n)
        .map(|i| w.read_mono_at(((i as f32 * ratio) as usize).min(frames - 1)))
        .collect();
    let want = (secs * SR) as usize;
    if all.len() <= want {
        return Some(all);
    }
    let step = (0.05 * SR) as usize;
    let energy = |s: usize| all[s..s + want].iter().map(|v| v * v).sum::<f32>();
    let best = (0..=(all.len() - want))
        .step_by(step.max(1))
        .max_by(|&a, &b| energy(a).total_cmp(&energy(b)))
        .unwrap_or(0);
    Some(all[best..best + want].to_vec())
}

// ---------------------------------------------------------------------------
// Moving pitch — the case a stationary probe cannot judge
// ---------------------------------------------------------------------------

/// A glottal voice whose pitch slides over `span` semitones across the take.
///
/// This is the material the epoch snap might exist for: the engine tracks a
/// smoothed `cur_t0` out of YIN, so on a moving pitch the nominal mark drifts
/// against the real pulse, and snapping could be compensating that error.
/// Nothing measured so far tests it, because [`noise_to_harmonic`] needs one
/// stationary f0.
pub fn glide(secs: f32, span_st: f32) -> Vec<f32> {
    glottal(secs, 0.02, 0.4, |t| F0 * (span_st * t / secs / 12.0).exp2())
}

/// A glottal voice with `depth` cents of vibrato at `rate` Hz — the ordinary
/// case of a sung note, and the one a stationary source silently omits.
pub fn vibrato(secs: f32, rate: f32, depth_cents: f32) -> Vec<f32> {
    glottal(secs, 0.02, 0.4, |t| {
        F0 * ((depth_cents / 1200.0) * (core::f32::consts::TAU * rate * t).sin()).exp2()
    })
}

/// How far the output is from a latency-aligned copy of the input, in dB
/// (residual energy over input energy). Lower is more transparent. Returns
/// the residual and the lag it was measured at.
///
/// The lag is FOUND, not assumed. An engine's real group delay is not
/// necessarily the latency it reports — PSOLA at a 70 Hz floor reports 2744
/// samples and actually delays by 2706, and 38 samples is 63 degrees of phase
/// at 220 Hz, enough to turn a −40 dB residual into +3 dB. A scalar gain is
/// least-squares fitted too, so this measures waveform difference rather than
/// level trim.
///
/// Unlike [`noise_to_harmonic`] this assumes nothing about the signal, so it
/// is the only transparency figure comparable across a held note, a glide and
/// a real sung phrase. Only valid at UNITY shift, where the ideal output is
/// the input itself.
pub fn unity_residual_db(
    input: &[f32],
    output: &[f32],
    nominal_lag: usize,
    skip: f32,
) -> (f32, usize) {
    let from = (skip * SR) as usize;
    let lo = nominal_lag.saturating_sub(400);
    let hi = nominal_lag + 400;
    let n = output.len().saturating_sub(from + hi).min((1.0 * SR) as usize);
    if n == 0 {
        return (f32::INFINITY, nominal_lag);
    }
    // Input energy does not depend on the lag, so it is computed once rather
    // than 801 times.
    let ea: f64 = (0..n).map(|i| (input[from + i] as f64).powi(2)).sum();
    let mut best = (nominal_lag, -1.0f64);
    for lag in lo..=hi {
        let (mut num, mut eb) = (0.0f64, 0.0f64);
        for i in 0..n {
            let a = input[from + i] as f64;
            let b = output[from + i + lag] as f64;
            num += a * b;
            eb += b * b;
        }
        let r = num / (ea * eb).sqrt().max(1e-30);
        if r > best.1 {
            best = (lag, r);
        }
    }
    let lag = best.0;
    // Least-squares gain, so a level trim does not read as distortion.
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for i in 0..n {
        let a = input[from + i] as f64;
        num += a * output[from + i + lag] as f64;
        den += a * a;
    }
    let g = if den > 0.0 { num / den } else { 1.0 };
    let (mut resid, mut energy) = (0.0f64, 0.0f64);
    for i in 0..n {
        let a = input[from + i] as f64;
        let d = output[from + i + lag] as f64 - g * a;
        resid += d * d;
        energy += (g * a) * (g * a);
    }
    (10.0 * (resid / energy.max(1e-30)).log10() as f32, lag)
}
