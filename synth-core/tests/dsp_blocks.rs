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
