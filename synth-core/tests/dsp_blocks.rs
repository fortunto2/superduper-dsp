//! Unit tests for shared DSP building blocks. These are intentionally
//! small/cheap — a regression here would simultaneously break every
//! SuperDuper effect that depends on synth-core, so we want them caught
//! at this layer before the per-plugin DSP tests even run.

use superduper_synth_core::dsp_blocks::{DcBlocker, Ducker, SmoothedParam, Tilt};

const SR: f32 = 48_000.0;

// ---------- DcBlocker ----------

#[test]
fn dc_blocker_removes_dc_offset() {
    let mut dc = DcBlocker::default();
    // Feed a sustained 1.0 (pure DC). After settling, output should be ≈0.
    let mut last = 1.0;
    for _ in 0..(SR as usize) {
        last = dc.process(1.0);
    }
    assert!(last.abs() < 0.01, "DC not removed after 1 s (residual = {last})");
}

#[test]
fn dc_blocker_passes_audio() {
    let mut dc = DcBlocker::default();
    let mut max_diff = 0.0_f32;
    for i in 0..1024 {
        let x = (i as f32 * 2.0 * core::f32::consts::PI * 1000.0 / SR).sin() * 0.5;
        let y = dc.process(x);
        // After transient, sine is preserved with virtually no attenuation.
        if i > 200 {
            max_diff = max_diff.max((x - y).abs());
        }
    }
    // R=0.995 has roughly 2.5% attenuation at 1 kHz — fine for music content.
    // Threshold here is mostly to catch a bug where the filter eats the signal.
    assert!(
        max_diff < 0.05,
        "DC blocker is attenuating a 1 kHz sine too much ({max_diff})"
    );
}

// ---------- SmoothedParam ----------

#[test]
fn smoothed_param_slews_toward_target() {
    let mut s = SmoothedParam::new(0.0);
    let mut hit_target = false;
    let mut prev = 0.0_f32;
    let mut overshoot = false;
    for _ in 0..(SR as usize / 10) {
        // 100 ms — well past the 5 ms time constant.
        let v = s.step(1.0, SR);
        // One-pole monotonically approaches target, never overshoots.
        if v > 1.0 + 1e-4 { overshoot = true; }
        // Must be strictly increasing while below target.
        if !hit_target && v < prev - 1e-6 {
            panic!("SmoothedParam not monotonic (prev={prev}, v={v})");
        }
        prev = v;
        if v > 0.999 { hit_target = true; }
    }
    assert!(hit_target, "SmoothedParam never reaches target");
    assert!(!overshoot, "SmoothedParam overshot 1.0");
}

#[test]
fn smoothed_param_snap_is_instant() {
    let mut s = SmoothedParam::new(0.0);
    s.snap(0.7);
    let v = s.step(0.7, SR);
    assert!((v - 0.7).abs() < 1e-6, "snap didn't take effect (v={v})");
}

// ---------- Tilt ----------

#[test]
fn tilt_zero_is_unity() {
    let mut t = Tilt::default();
    let x = 0.5;
    // Warm the LPF up so the transient settles.
    for _ in 0..1000 {
        t.process(x, SR, 0.0);
    }
    let y = t.process(x, SR, 0.0);
    assert!(
        (y - x).abs() < 1e-3,
        "tilt=0 should be unity gain (out={y}, in={x})"
    );
}

#[test]
fn tilt_extremes_change_balance() {
    // Mid-frequency tone — at +1 tilt should be louder than at -1.
    let mut t_up = Tilt::default();
    let mut t_dn = Tilt::default();
    let mut sum_sq_up = 0.0_f32;
    let mut sum_sq_dn = 0.0_f32;
    for i in 0..4096 {
        let x = (i as f32 * 2.0 * core::f32::consts::PI * 4000.0 / SR).sin();
        let yu = t_up.process(x, SR, 1.0);
        let yd = t_dn.process(x, SR, -1.0);
        sum_sq_up += yu * yu;
        sum_sq_dn += yd * yd;
    }
    assert!(
        sum_sq_up > sum_sq_dn * 1.5,
        "tilt direction has no effect (up={sum_sq_up} down={sum_sq_dn})"
    );
}

// ---------- Ducker ----------

#[test]
fn ducker_attenuates_under_load() {
    let mut d = Ducker::default();
    let mut g = 1.0;
    // Hammer envelope with full-scale key for ~500 ms (well past attack).
    for _ in 0..(SR as usize / 2) {
        g = d.process(1.0, 1.0, SR, 12.0, 5.0, 100.0);
    }
    // 12 dB amount at envelope ≈ 1.0 → gain ≈ 0.25.
    assert!(g < 0.35, "ducker didn't attenuate (gain={g})");
}

#[test]
fn ducker_recovers_after_silence() {
    let mut d = Ducker::default();
    for _ in 0..(SR as usize / 2) {
        d.process(1.0, 1.0, SR, 12.0, 5.0, 100.0);
    }
    // After 500 ms of silence — gain should recover near unity.
    let mut g = 0.0;
    for _ in 0..(SR as usize / 2) {
        g = d.process(0.0, 0.0, SR, 12.0, 5.0, 100.0);
    }
    assert!(g > 0.9, "ducker release stuck (gain={g})");
}

#[test]
fn ducker_zero_amount_is_unity() {
    let mut d = Ducker::default();
    for _ in 0..1000 {
        let g = d.process(1.0, 1.0, SR, 0.0, 5.0, 100.0);
        assert!((g - 1.0).abs() < 1e-6, "amount=0 should be unity (g={g})");
    }
}

// ---------- AdsrEnvelope + midi_note_to_hz ----------

#[test]
fn adsr_starts_idle_and_stays_silent() {
    use superduper_synth_core::dsp_blocks::{AdsrEnvelope, AdsrParams};
    let mut env = AdsrEnvelope::default();
    assert!(env.is_idle());
    let p = AdsrParams { sr: SR, delay_s: 0.0, attack_s: 0.1, hold_s: 0.0, decay_s: 0.1, sustain: 0.5, release_s: 0.1 };
    for _ in 0..1000 {
        assert_eq!(env.process(p), 0.0, "idle envelope must stay silent");
    }
}

#[test]
fn adsr_release_then_idle_within_5_release_constants() {
    use superduper_synth_core::dsp_blocks::{AdsrEnvelope, AdsrParams};
    let mut env = AdsrEnvelope::default();
    env.gate_on();
    let p = AdsrParams { sr: SR, delay_s: 0.0, attack_s: 0.001, hold_s: 0.0, decay_s: 0.001, sustain: 0.5, release_s: 0.05 };
    // run to sustain
    for _ in 0..(SR as usize / 10) { env.process(p); }
    env.gate_off();
    // RELEASE_FLOOR is 1e-4 of unity. Crossing it from sustain 0.5 with a
    // one-pole exponential needs ~9·τ; give a 15·τ safety margin.
    let n = (15.0 * 0.05 * SR) as usize;
    let mut became_idle = false;
    for _ in 0..n {
        env.process(p);
        if env.is_idle() { became_idle = true; break; }
    }
    assert!(became_idle, "envelope should idle within 15*release seconds");
}

#[test]
fn midi_note_to_hz_roundtrip() {
    use superduper_synth_core::dsp_blocks::midi_note_to_hz;
    // Octave ratios.
    for &(low, high) in &[(48.0_f32, 60.0_f32), (60.0, 72.0), (24.0, 36.0)] {
        let r = midi_note_to_hz(high) / midi_note_to_hz(low);
        assert!((r - 2.0).abs() < 1e-4, "{low}→{high}: ratio {r} not 2.0");
    }
}

// ── BS.1770 LUFS meter — EBU Tech 3341 reference cases ─────────────────────
// Case 1: 997 Hz stereo sine, −23 dBFS per channel, → I = −23.0 LUFS ±0.1.
// Regression for the channel-summation bug: averaging L/R mean-squares
// (instead of SUMMING per BS.1770-4 §5.6) read −26.0 — 3.01 dB low — which
// mis-calibrated every master downstream (limiters driven 3 dB too hard).
#[test]
fn lufs_stereo_sine_reads_minus_23() {
    use superduper_synth_core::loudness::LoudnessMeter;
    const SR: f32 = 48_000.0;
    let mut m = LoudnessMeter::new(SR);
    let amp = 10f32.powf(-23.0 / 20.0);
    let n = (10.0 * SR) as usize;
    for i in 0..n {
        let s = amp * (2.0 * std::f32::consts::PI * 997.0 * i as f32 / SR).sin();
        m.process_stereo(s, s);
    }
    let i = m.integrated_lufs();
    assert!(
        (i - -23.0).abs() < 0.15,
        "stereo 997 Hz −23 dBFS sine must read −23.0 LUFS (BS.1770-4 channel sum), got {i}"
    );
    let st = m.short_term_lufs();
    assert!((st - -23.0).abs() < 0.15, "short-term should also read −23.0, got {st}");
}

// One-sided signal (L only, R silent): z_R = 0 contributes nothing, no
// division by channel count — expect −26.0 LUFS for a −23 dBFS mono-in-L sine.
#[test]
fn lufs_left_only_sine_reads_minus_26() {
    use superduper_synth_core::loudness::LoudnessMeter;
    const SR: f32 = 48_000.0;
    let mut m = LoudnessMeter::new(SR);
    let amp = 10f32.powf(-23.0 / 20.0);
    let n = (10.0 * SR) as usize;
    for i in 0..n {
        let s = amp * (2.0 * std::f32::consts::PI * 997.0 * i as f32 / SR).sin();
        m.process_stereo(s, 0.0);
    }
    let i = m.integrated_lufs();
    assert!(
        (i - -26.0).abs() < 0.15,
        "left-only −23 dBFS sine must read −26.0 LUFS, got {i}"
    );
}

// ── True-peak: fs/4 sine with π/4 phase — samples land at ±0.707 (−3.01
// dBFS sample peak) while the continuous waveform peaks at 1.0 (0 dBTP).
// Linear interpolation between samples can NEVER exceed the sample max, so
// the old detector read −3.0; a proper 4× FIR must read ≈ 0 dBTP.
#[test]
fn true_peak_catches_intersample_overshoot() {
    use superduper_synth_core::loudness::TruePeakDetector;
    let mut tp = TruePeakDetector::new();
    for i in 0..48_000usize {
        let s = (std::f32::consts::PI * 0.5 * i as f32 + std::f32::consts::PI * 0.25).sin();
        tp.process_stereo(s, s);
    }
    let db = tp.dbtp();
    assert!(
        db > -0.5,
        "fs/4 sine @ π/4 phase true-peaks at 0 dBTP; detector saw only {db} dBTP"
    );
}

// Two-sided oracle. The test above is a one-sided threshold (`> -0.5`), so
// it passes just as happily on a detector that over-reports by 5 dB — and
// over-reporting is exactly what a short windowed-sinc does when its DC
// normalization overshoots. The analytic answer here is known, so assert
// the value, not an inequality: an fs/4 sine at phase π/4 and amplitude
// 0.5 has samples at ±0.3536 and a true peak of exactly 0.5, i.e. -6.0206
// dBTP, 3.01 dB above sample peak.
//
// Faded in over 100 samples on purpose: a hard start is a step, and a step
// genuinely rings in the band-limited reconstruction (~+0.11 dB measured),
// which would be a property of the test signal, not of the detector.
#[test]
fn true_peak_matches_analytic_oracle_both_ways() {
    use superduper_synth_core::loudness::TruePeakDetector;
    let mut tp = TruePeakDetector::new();
    for i in 0..48_000usize {
        let fade = (i as f32 / 100.0).min(1.0);
        let s = 0.5 * fade
            * (std::f32::consts::PI * 0.5 * i as f32 + std::f32::consts::PI * 0.25).sin();
        tp.process_stereo(s, s);
    }
    let db = tp.dbtp();
    assert!(
        (db - -6.0206).abs() < 0.1,
        "fs/4 sine @ π/4, A=0.5 true-peaks at exactly -6.0206 dBTP; got {db}"
    );
}

// DC-ish input must not report phantom overshoot (interpolator
// normalization). Fade the DC in over 100 samples — a hard 0→0.5 step
// genuinely true-peaks above its plateau (Gibbs in the bandlimited
// reconstruction), which is not what this test is about.
#[test]
fn true_peak_no_phantom_overshoot_on_dc() {
    use superduper_synth_core::loudness::TruePeakDetector;
    let mut tp = TruePeakDetector::new();
    for i in 0..4_800usize {
        let s = 0.5 * (i as f32 / 100.0).min(1.0);
        tp.process_stereo(s, s);
    }
    let db = tp.dbtp();
    assert!((db - -6.02).abs() < 0.15, "ramped DC 0.5 must read ≈ −6.02 dBTP, got {db}");
}

// ---------- FormantTracker ----------

/// Band-limited glottal-ish pulse train: harmonics of `f0` with 1/k rolloff,
/// stopping below 5 kHz so the tracker's search ranges see clean structure.
fn pulse_train(f0: f32, sr: f32, n: usize) -> Vec<f32> {
    let kmax = ((sr * 0.45).min(5_000.0) / f0) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / sr;
            let mut s = 0.0;
            for k in 1..=kmax {
                s += (std::f32::consts::TAU * f0 * k as f32 * t).sin() / k as f32;
            }
            s * 0.3
        })
        .collect()
}

/// The tracker must recover the formants a known vowel filter imposed. This is
/// the contract SuperDuper Formant's Follow mode rests on: sing a vowel, get
/// its F1/F2/F3 back so another sound can be articulated with them.
#[test]
fn formant_tracker_recovers_a_known_vowel() {
    use superduper_synth_core::formant::{Formant, FORMANT_PRESETS};
    use superduper_synth_core::formant_track::FormantTracker;

    // FORMANT_PRESETS[1] = Vowel A (/ɑ/) — 730 / 1090 / 2440 Hz.
    let vowel = FORMANT_PRESETS[1];
    let src = pulse_train(120.0, SR, SR as usize / 2); // 0.5 s of "aaah"
    let mut filt = Formant::default();
    let mut tracker = FormantTracker::new(SR);
    for &s in &src {
        let (l, _r) = filt.process(s, s, SR, vowel.f, vowel.bw, vowel.gain, 1.0);
        tracker.push(l, 25.0, -60.0);
    }
    let got = tracker.formants();
    assert!(tracker.is_active(), "0.5 s of vowel at -10 dBFS must open the gate");
    for i in 0..3 {
        let want = vowel.f[i];
        let err = (got[i] - want).abs() / want;
        assert!(
            err < 0.15,
            "F{} off by {:.1}% — wanted {want:.0} Hz, tracked {:.0} Hz (all: {got:?})",
            i + 1,
            err * 100.0,
            got[i]
        );
    }
}

/// Two different vowels must land in different places — a tracker that always
/// reports the same triple would pass the test above and still be useless.
#[test]
fn formant_tracker_separates_two_vowels() {
    use superduper_synth_core::formant::{Formant, FORMANT_PRESETS};
    use superduper_synth_core::formant_track::FormantTracker;

    let track_vowel = |p: superduper_synth_core::formant::FormantPreset| {
        let src = pulse_train(140.0, SR, SR as usize / 2);
        let mut filt = Formant::default();
        let mut tr = FormantTracker::new(SR);
        for &s in &src {
            let (l, _) = filt.process(s, s, SR, p.f, p.bw, p.gain, 1.0);
            tr.push(l, 25.0, -60.0);
        }
        tr.formants()
    };
    // /i/ = 270 / 2290 (closed, bright) vs /ɔ/ = 570 / 840 (open, dark).
    let i_vowel = track_vowel(FORMANT_PRESETS[3]);
    let o_vowel = track_vowel(FORMANT_PRESETS[4]);
    assert!(
        i_vowel[0] < o_vowel[0],
        "/i/ F1 ({:.0}) must sit below /ɔ/ F1 ({:.0})",
        i_vowel[0],
        o_vowel[0]
    );
    assert!(
        i_vowel[1] > o_vowel[1] * 1.5,
        "/i/ F2 ({:.0}) must sit far above /ɔ/ F2 ({:.0})",
        i_vowel[1],
        o_vowel[1]
    );
}

/// Silence must freeze the last estimate, not collapse it — a breath between
/// words should hold the vowel, not snap the articulation to the noise floor.
#[test]
fn formant_tracker_freezes_below_gate() {
    use superduper_synth_core::formant::{Formant, FORMANT_PRESETS};
    use superduper_synth_core::formant_track::FormantTracker;

    let vowel = FORMANT_PRESETS[3]; // /i/ — far from the neutral start value
    let src = pulse_train(120.0, SR, SR as usize / 2);
    let mut filt = Formant::default();
    let mut tr = FormantTracker::new(SR);
    for &s in &src {
        let (l, _) = filt.process(s, s, SR, vowel.f, vowel.bw, vowel.gain, 1.0);
        tr.push(l, 25.0, -60.0);
    }
    let before = tr.formants();
    for _ in 0..(SR as usize / 4) {
        tr.push(0.0, 25.0, -60.0);
    }
    let after = tr.formants();
    assert!(!tr.is_active(), "silence must close the gate");
    // 2 % tolerance, not bit-equality: the first frame after the cut still
    // holds the tail samples that hadn't yet reached a hop boundary, so it
    // legitimately analyses real signal one last time.
    for i in 0..3 {
        assert!(
            (after[i] - before[i]).abs() < before[i] * 0.02,
            "F{} drifted during silence: {:.0} → {:.0}",
            i + 1,
            before[i],
            after[i]
        );
    }
}

// ---------------------------------------------------------------------------
// YinPitchTracker — candidate scoring (octave stability + jump following)
// ---------------------------------------------------------------------------

fn saw(f0: f32, sr: f32, n: usize, amp: impl Fn(usize) -> f32) -> Vec<f32> {
    let mut phase = 0.0f32;
    (0..n)
        .map(|i| {
            phase = (phase + f0 / sr).fract();
            amp(i) * (2.0 * phase - 1.0)
        })
        .collect()
}

#[test]
fn pitch_tracker_holds_octave_through_tremolo() {
    use superduper_synth_core::pitch::YinPitchTracker;
    let sr = 48_000.0;
    let f0 = 200.0;
    // Amplitude tremolo is the classic octave-flip provocation: the AM
    // sidebands make the 2·T valley competitive with the true period.
    let sig = saw(f0, sr, 2 * 48_000, |i| {
        0.55 + 0.45 * (i as f32 * 6.0 / sr * core::f32::consts::TAU).sin()
    });
    let mut tr = YinPitchTracker::new(sr, 70.0, 1000.0, 1536, 256, 150.0);
    let mut estimates = Vec::new();
    for (i, &x) in sig.iter().enumerate() {
        if tr.push(x) && i > 24_000 {
            estimates.push(tr.current_hz());
        }
    }
    assert!(estimates.len() > 100, "tracker produced too few estimates");
    for hz in estimates {
        let st = (hz / f0).log2().abs() * 12.0;
        assert!(st < 0.8, "octave/step error: {hz:.1} Hz vs {f0} Hz ({st:.2} st)");
    }
}

#[test]
fn pitch_tracker_follows_a_real_jump() {
    use superduper_synth_core::pitch::YinPitchTracker;
    let sr = 48_000.0;
    let a = saw(220.0, sr, 48_000, |_| 0.8);
    let b = saw(330.0, sr, 48_000, |_| 0.8);
    let mut tr = YinPitchTracker::new(sr, 70.0, 1000.0, 1536, 256, 150.0);
    for &x in &a {
        tr.push(x);
    }
    // The continuity pull must slow a legitimate jump by hops, not block it.
    let mut caught_after = None;
    let mut hops = 0;
    for &x in &b {
        if tr.push(x) {
            hops += 1;
            if caught_after.is_none() && (tr.current_hz() / 330.0).log2().abs() * 12.0 < 0.6 {
                caught_after = Some(hops);
            }
        }
    }
    let caught = caught_after.expect("tracker never reached the new pitch");
    assert!(caught <= 30, "took {caught} hops (>160 ms) to follow a fifth up");
    let final_hz = tr.current_hz();
    assert!(
        ((final_hz / 330.0).log2().abs() * 12.0) < 0.6,
        "settled at {final_hz:.1} Hz instead of 330"
    );
}

// ---------------------------------------------------------------------------
// SwiftF0 — streaming Rust inference vs the reference ONNX model (golden)
// ---------------------------------------------------------------------------

#[test]
fn swiftf0_streaming_matches_reference_model() {
    use superduper_synth_core::swiftf0::SwiftF0Tracker;
    let load = |b: &'static [u8]| -> Vec<f32> {
        b.chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    };
    let audio = load(include_bytes!("data/swiftf0_golden_audio.bin"));
    let ref_hz = load(include_bytes!("data/swiftf0_golden_hz.bin"));
    let ref_conf = load(include_bytes!("data/swiftf0_golden_conf.bin"));

    let mut tr = SwiftF0Tracker::new(16_000.0, 150.0);
    let mut hz = Vec::new();
    let mut conf = Vec::new();
    let t0 = std::time::Instant::now();
    for &x in &audio {
        if tr.push(x) {
            hz.push(tr.current_hz());
            conf.push(tr.confidence());
        }
    }
    let el = t0.elapsed().as_secs_f64();
    let dur = audio.len() as f64 / 16_000.0;
    println!("swiftf0 rust inference: {:.1}x realtime ({:.2} ms per s of audio)",
             dur / el, el / dur * 1000.0);
    assert!(hz.len() + 2 >= ref_hz.len(), "frame count: {} vs {}", hz.len(), ref_hz.len());

    // Streaming computes every column with the future taps zeroed, so it is
    // CAUSAL where the reference is zero-phase (its conv context reaches ±10
    // frames into the future) — on a sweeping pitch that reads as a constant
    // group delay, not an error. Align by the best constant lag (bounded),
    // then bound the residual in cents where the reference is voiced.
    let n = hz.len().min(ref_hz.len());
    let stats = |lag: usize| -> (f32, f32) {
        let mut cents: Vec<f32> = (lag..n)
            .filter(|&i| ref_conf[i - lag] > 0.6)
            .map(|i| ((hz[i] / ref_hz[i - lag]).log2() * 1200.0).abs())
            .collect();
        cents.sort_by(f32::total_cmp);
        (cents[cents.len() / 2], cents[cents.len() * 95 / 100])
    };
    let (lag, (med, p95)) = (0..6usize)
        .map(|l| (l, stats(l)))
        .min_by(|a, b| a.1 .0.total_cmp(&b.1 .0))
        .unwrap();
    // Steady-pitch frames are the ones a correction decision hangs on; the
    // sweep residual is delay semantics, not accuracy.
    let mut steady: Vec<f32> = (lag..n)
        .filter(|&i| {
            i >= lag + 1
                && ref_conf[i - lag] > 0.6
                && (ref_hz[i - lag] - ref_hz[i - lag - 1]).abs() < 1.0
        })
        .map(|i| ((hz[i] / ref_hz[i - lag]).log2() * 1200.0).abs())
        .collect();
    steady.sort_by(f32::total_cmp);
    let smed = steady[steady.len() / 2];
    let sp95 = steady[steady.len() * 95 / 100];
    println!(
        "swiftf0 golden: lag {lag}, overall median {med:.2} / p95 {p95:.2} cents,          steady ({}) median {smed:.2} / p95 {sp95:.2} cents",
        steady.len()
    );
    // Thresholds are the measured causal-streaming characteristic plus
    // headroom, not an aspiration: the offline reference sees ±10 frames of
    // future context (5 layers × ±2), so matching it exactly would cost
    // 160 ms of latency. ~6 cents on steady notes is the price of running
    // live, and it sits at the edge of pitch JND — a regression past these
    // numbers means the port broke, not that streaming got worse.
    assert!(lag <= 4, "group delay too large: {lag} frames");
    assert!(smed < 10.0, "steady median {smed:.2} cents");
    assert!(sp95 < 40.0, "steady p95 {sp95:.2} cents");
    assert!(med < 30.0, "overall median {med:.2} cents at lag {lag}");

    let dconf: f32 = (lag..n)
        .map(|i| (conf[i] - ref_conf[i - lag]).abs())
        .sum::<f32>()
        / (n - lag) as f32;
    println!("swiftf0 golden: mean |dConf| {dconf:.3}");
    assert!(dconf < 0.15, "confidence drifted: {dconf:.3}");
}

#[test]
fn swiftf0_resampler_tracks_a_saw_at_48k() {
    use superduper_synth_core::swiftf0::SwiftF0Tracker;
    let sr = 48_000.0;
    let mut tr = SwiftF0Tracker::new(sr, 150.0);
    let mut phase = 0.0f32;
    let mut est = Vec::new();
    for _ in 0..(2 * 48_000) {
        phase = (phase + 220.0 / sr).fract();
        if tr.push(0.6 * (2.0 * phase - 1.0)) && tr.confidence() > 0.5 {
            est.push(tr.current_hz());
        }
    }
    assert!(est.len() > 60, "too few voiced estimates: {}", est.len());
    est.sort_by(f32::total_cmp);
    let med = est[est.len() / 2];
    assert!((med - 220.0).abs() < 1.5, "median {med:.2} Hz, want 220");
}

// ---------------------------------------------------------------------------
// epoch_sharpness — the descriptor that decides which pitch engine runs
// ---------------------------------------------------------------------------

use sdsp_test_kit::signals as common;

/// The number itself. Printed so the doc-comment table in `pitch.rs` can be
/// checked against reality without re-deriving it by hand.
fn sharpness_of(x: &[f32]) -> f32 {
    use superduper_synth_core::pitch::epoch_sharpness;
    let t0 = (common::SR / common::F0).round() as usize;
    // Measure over the steady middle, the way a running tracker would see it:
    // one reading per period, take the median so a single odd window can't
    // decide an engine.
    let mut vals: Vec<f32> = (0..40)
        .map(|k| {
            let end = (0.8 * common::SR) as usize + k * t0;
            epoch_sharpness(&x[end - 6 * t0..end], t0)
        })
        .collect();
    vals.sort_by(f32::total_cmp);
    vals[vals.len() / 2]
}

#[test]
fn epoch_sharpness_separates_pulsed_from_smooth() {
    let pulsed = sharpness_of(&common::pulsed(2.0));
    let smooth = sharpness_of(&common::smooth(2.0));
    println!("epoch sharpness: pulsed {pulsed:.2}, smooth {smooth:.2}");
    // The plan's bar. PSOLA is transparent on the first and 64 dB worse on
    // the second, so a descriptor that cannot tell them apart is useless.
    assert!(
        pulsed > 2.0 * smooth.max(0.05),
        "need a clear margin, got pulsed {pulsed:.2} vs smooth {smooth:.2}"
    );
}

/// The finding that shaped the threshold: a breathy voice is unambiguously
/// voiced and still has no epoch worth snapping to, so it must land on the
/// pvoc side of the line with the synth tone, not on the PSOLA side with the
/// other voice.
#[test]
fn epoch_sharpness_puts_a_breathy_voice_with_the_smooth_tone() {
    let normal = sharpness_of(&common::voiced(2.0));
    let breathy = sharpness_of(&common::breathy(2.0));
    println!("epoch sharpness: voiced {normal:.2}, breathy {breathy:.2}");
    assert!(normal > 0.9, "a normal sung note must read as pulsed: {normal:.2}");
    assert!(breathy < 0.9, "a breathy take must read as smooth: {breathy:.2}");
    assert!(normal > 1.9 * breathy, "margin too thin: {normal:.2} vs {breathy:.2}");
}

#[test]
fn epoch_sharpness_is_zero_on_nothing_to_measure() {
    use superduper_synth_core::pitch::epoch_sharpness;
    let x = common::pulsed(0.5);
    assert_eq!(epoch_sharpness(&x, 0), 0.0, "t0 = 0 must not divide by zero");
    assert_eq!(epoch_sharpness(&x[..100], 218), 0.0, "window shorter than 3 periods");
    assert_eq!(epoch_sharpness(&vec![0.0; 2048], 218), 0.0, "silence has no epoch");
}
