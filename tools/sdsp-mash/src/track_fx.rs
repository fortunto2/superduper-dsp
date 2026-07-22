//! Per-track FX applied to a stem before it's placed on the grid — currently
//! a high-pass (RBJ biquad) and a linked feed-forward compressor, aimed at
//! cleaning up and evening out a vocal a-cappella.
//!
//! Not real-time; runs offline over the whole stem, so a plain per-sample
//! loop is fine.

use superduper_synth_core::dsp_blocks::{compressor_gain_db, tape_clip, Biquad, EnvelopeDetector};

use crate::config::TrackCompConfig;
use crate::duck::{db_to_lin, lin_to_db};

/// Vocal delay-throw (v2.3): the last word of each phrase is thrown into a
/// dotted-quarter feedback echo that rings into the following pause — the
/// dubby Fred-again vocal tail. Phrase boundaries are detected as loud→quiet
/// transitions where the quiet holds for > `gap_sec` (a breath between lines).
/// Additive, so the dry vocal is untouched.
pub fn apply_delay_throw(l: &mut [f32], r: &mut [f32], sr: u32, bpm: f64, feedback: f32) {
    let n = l.len().min(r.len());
    if n == 0 || bpm <= 0.0 {
        return;
    }
    let beat = 60.0 / bpm * sr as f64;
    let delay = (1.5 * beat).round() as usize; // dotted quarter
    let throw = ((sr as f64) * 0.32).round() as usize; // the last "word"
    let fb = feedback.clamp(0.0, 0.85);
    if delay == 0 || throw == 0 {
        return;
    }

    // RMS envelope at ~50 Hz.
    let hop = (sr as usize / 50).max(1);
    let frames = n / hop;
    if frames < 8 {
        return;
    }
    let mut env = vec![0.0f32; frames];
    let mut peak = 1e-9f32;
    for (f, e) in env.iter_mut().enumerate() {
        let base = f * hop;
        let mut sq = 0.0f32;
        for k in 0..hop {
            let v = 0.5 * (l[base + k] + r[base + k]);
            sq += v * v;
        }
        *e = (sq / hop as f32).sqrt();
        peak = peak.max(*e);
    }
    let thr = peak * 0.14;
    let gap_frames = ((0.4 * 50.0) as usize).max(1); // 0.4 s of silence
    let min_spacing = (2.0 * beat) as usize; // don't throw more than every ~2 beats

    // Phrase-ends: loud frame followed by ≥gap_frames below threshold.
    let mut ends: Vec<usize> = Vec::new();
    let mut f = 1usize;
    while f + gap_frames < frames {
        if env[f - 1] > thr && env[f] <= thr && env[f..f + gap_frames].iter().all(|&e| e <= thr) {
            let pe = f * hop;
            if ends.last().map_or(true, |&last| pe > last + min_spacing) {
                ends.push(pe);
            }
        }
        f += 1;
    }

    // Throw each phrase-tail into decaying echoes across the pause.
    for &pe in &ends {
        if pe < throw {
            continue;
        }
        let seed_l: Vec<f32> = l[pe - throw..pe].to_vec();
        let seed_r: Vec<f32> = r[pe - throw..pe].to_vec();
        let mut k = 1usize;
        loop {
            let g = fb.powi(k as i32);
            if g < 0.06 {
                break;
            }
            let base = pe + (k - 1) * delay;
            if base >= n {
                break;
            }
            for j in 0..throw {
                let d = base + j;
                if d < n {
                    l[d] += seed_l[j] * g;
                    r[d] += seed_r[j] * g;
                }
            }
            k += 1;
        }
    }
}

/// Tape saturation on a stem in place — fattens a bass (`drive_db` ≈ 3–6).
/// Drives into `tape_clip`'s soft curve, then trims back so the level is
/// roughly preserved (the curve adds harmonics + compresses peaks).
pub fn apply_saturate(l: &mut [f32], r: &mut [f32], drive_db: f64) {
    let drive = db_to_lin(drive_db as f32).max(1.0);
    // Rough make-down so the saturated signal isn't much louder than dry.
    let comp = 1.0 / (1.0 + 0.35 * (drive - 1.0));
    let n = l.len().min(r.len());
    for i in 0..n {
        l[i] = tape_clip(l[i], drive) * comp;
        r[i] = tape_clip(r[i], drive) * comp;
    }
}

/// Wide musical Q for the presence bell — covers roughly 2–4 kHz at 3 kHz.
const PRESENCE_Q: f32 = 0.8;

/// Presence boost: a peaking bell at `hz` (typically 3 kHz) lifting the
/// speech-intelligibility band so a rap vocal cuts through a dense breakbeat.
pub fn apply_presence(l: &mut [f32], r: &mut [f32], sr: u32, hz: f64, gain_db: f64) {
    let mut fl = Biquad::default();
    let mut fr = Biquad::default();
    fl.set_peaking(sr as f32, hz as f32, PRESENCE_Q, gain_db as f32);
    fr.set_peaking(sr as f32, hz as f32, PRESENCE_Q, gain_db as f32);
    let n = l.len().min(r.len());
    for i in 0..n {
        l[i] = fl.process(l[i]);
        r[i] = fr.process(r[i]);
    }
}

/// Butterworth-ish Q for the corrective high-pass.
const HP_Q: f32 = 0.707;

/// High-pass both channels in place at `hz`.
pub fn apply_highpass(l: &mut [f32], r: &mut [f32], sr: u32, hz: f64) {
    let mut fl = Biquad::default();
    let mut fr = Biquad::default();
    fl.set_hpf(sr as f32, hz as f32, HP_Q);
    fr.set_hpf(sr as f32, hz as f32, HP_Q);
    let n = l.len().min(r.len());
    for i in 0..n {
        l[i] = fl.process(l[i]);
        r[i] = fr.process(r[i]);
    }
}

/// Compress both channels in place with a stereo-linked detector (so the
/// image doesn't wander). Same soft-knee curve as the master compressor.
pub fn apply_comp(l: &mut [f32], r: &mut [f32], sr: u32, c: &TrackCompConfig) {
    let mut det = EnvelopeDetector::default();
    let makeup = db_to_lin(c.makeup_db as f32);
    let sr_f = sr as f32;
    let (thr, ratio, atk, rel, knee) = (
        c.threshold_db as f32,
        c.ratio as f32,
        c.attack_ms as f32,
        c.release_ms as f32,
        c.knee_db as f32,
    );
    let n = l.len().min(r.len());
    for i in 0..n {
        let key = l[i].abs().max(r[i].abs());
        let env = det.process(key, sr_f, atk, rel);
        let env_db = lin_to_db(env);
        let gr_db = compressor_gain_db(env_db, thr, ratio, knee);
        let g = db_to_lin(gr_db) * makeup;
        l[i] *= g;
        r[i] *= g;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;

    fn sine(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / SR as f32).sin())
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn highpass_cuts_lows_keeps_highs() {
        let n = SR as usize;
        let mut lo_l = sine(50.0, 0.5, n);
        let mut lo_r = lo_l.clone();
        apply_highpass(&mut lo_l, &mut lo_r, SR, 200.0);

        let mut hi_l = sine(2000.0, 0.5, n);
        let mut hi_r = hi_l.clone();
        apply_highpass(&mut hi_l, &mut hi_r, SR, 200.0);

        // Skip the filter warm-up.
        let lo = rms(&lo_l[2000..]);
        let hi = rms(&hi_l[2000..]);
        assert!(lo < 0.15, "50 Hz should be cut by a 200 Hz HP, got {lo}");
        assert!(hi > 0.3, "2 kHz should pass a 200 Hz HP, got {hi}");
    }

    #[test]
    fn presence_boosts_speech_band_leaves_lows_and_air() {
        let n = SR as usize;
        let mk = |hz: f32| {
            let mut l = sine(hz, 0.3, n);
            let mut r = l.clone();
            apply_presence(&mut l, &mut r, SR, 3000.0, 3.0);
            20.0 * (rms(&l[2000..]) / rms(&sine(hz, 0.3, n)[2000..])).log10()
        };
        let at_3k = mk(3000.0);
        let at_300 = mk(300.0);
        let at_12k = mk(12000.0);
        assert!((at_3k - 3.0).abs() < 0.5, "3 kHz should get ≈+3 dB, got {at_3k}");
        assert!(at_300.abs() < 0.5, "300 Hz should be untouched, got {at_300}");
        assert!(at_12k.abs() < 0.7, "12 kHz should be near-untouched, got {at_12k}");
    }

    #[test]
    fn comp_reduces_dynamic_range() {
        // A signal that steps from quiet to loud; after compression the loud
        // part should be closer in level to the quiet part.
        let n = SR as usize;
        let mut l = vec![0.0f32; n];
        for i in 0..n / 2 {
            l[i] = 0.1 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / SR as f32).sin();
        }
        for i in n / 2..n {
            l[i] = 0.9 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / SR as f32).sin();
        }
        let mut r = l.clone();
        let quiet_before = rms(&l[SR as usize / 8..SR as usize / 4]);
        let loud_before = rms(&l[n / 2 + SR as usize / 8..n / 2 + SR as usize / 4]);

        let c = TrackCompConfig {
            threshold_db: -24.0,
            ratio: 4.0,
            attack_ms: 5.0,
            release_ms: 100.0,
            knee_db: 6.0,
            makeup_db: 0.0,
        };
        apply_comp(&mut l, &mut r, SR, &c);
        let quiet_after = rms(&l[SR as usize / 8..SR as usize / 4]);
        let loud_after = rms(&l[n / 2 + SR as usize / 8..n / 2 + SR as usize / 4]);

        let ratio_before = loud_before / quiet_before;
        let ratio_after = loud_after / quiet_after;
        assert!(
            ratio_after < ratio_before * 0.8,
            "compressor should shrink the loud/quiet ratio: before {ratio_before}, after {ratio_after}"
        );
    }

    #[test]
    fn saturate_adds_harmonics_without_blowing_level() {
        // A pure 80 Hz bass tone → tape drive should add harmonic energy
        // (2nd/3rd) while keeping RMS in the same ballpark.
        let n = SR as usize;
        let mut l = sine(80.0, 0.5, n);
        let mut r = l.clone();
        let before = rms(&l);
        apply_saturate(&mut l, &mut r, 5.0);
        let after = rms(&l);
        // Level preserved within ~4 dB.
        assert!((after / before).clamp(0.0, 10.0) > 0.63, "sat killed the level: {before}→{after}");
        assert!(after / before < 1.6, "sat too loud: {before}→{after}");
        // Harmonics: tape_clip is an odd function → adds the 3rd harmonic
        // (240 Hz). Measure both quadratures so phase doesn't hide it.
        let (mut hs, mut hc) = (0.0f32, 0.0f32);
        for i in 0..n {
            let w = 2.0 * std::f32::consts::PI * 240.0 * i as f32 / SR as f32;
            hs += l[i] * w.sin();
            hc += l[i] * w.cos();
        }
        let h3 = (hs * hs + hc * hc).sqrt() / n as f32;
        assert!(h3 > 1e-3, "tape drive should add a 3rd harmonic ({h3})");
    }

    #[test]
    fn delay_throw_rings_the_phrase_tail_into_the_pause() {
        // A "word" (loud tone) for 0.5 s, then ~1.5 s of silence (a phrase
        // gap). The throw should fill the pause with decaying echoes.
        let sr = SR;
        let n = sr as usize * 2;
        let mut l = vec![0.0f32; n];
        let word = sr as usize / 2;
        for i in 0..word {
            l[i] = 0.6 * (2.0 * std::f32::consts::PI * 400.0 * i as f32 / sr as f32).sin();
        }
        let mut r = l.clone();
        apply_delay_throw(&mut l, &mut r, sr, 120.0, 0.5);
        // The pause (just after the word) should now carry echo energy…
        let pause_e = rms(&l[word + 1000..word + sr as usize / 2]);
        assert!(pause_e > 1e-3, "throw should ring into the pause ({pause_e})");
        // …decaying: energy right after the word > energy later in the pause.
        let near = rms(&l[word..word + sr as usize / 4]);
        let far = rms(&l[word + 3 * sr as usize / 4..n]);
        assert!(far < near, "echoes should decay: near {near}, far {far}");
    }
}
