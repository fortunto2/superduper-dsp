//! Wave's mailbox document (`superduper_synth_core::bus_mail`): the drawn
//! wavetable, readable and writable by an agent in the terms mixing needs —
//! harmonics. "Keep the fundamental and 2nd, drop the 3rd that lands on the
//! guitar" is one `set`.

use crate::{osc, push_custom_frames, SharedParamsInner, FRAMES_MAX};
use serde_json::{json, Value};

/// Harmonics reported and accepted per frame.
pub const HARMONICS: usize = 32;
const PREVIEW: usize = 64;

/// Amplitude and phase of harmonics 1..=HARMONICS of one table (a sine of
/// peak 1 at harmonic k reads amplitude 1 at k).
fn analyse(table: &[f32]) -> (Vec<f32>, Vec<f32>) {
    let n = table.len() as f32;
    (1..=HARMONICS)
        .map(|k| {
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for (i, x) in table.iter().enumerate() {
                let w = 2.0 * std::f32::consts::PI * k as f32 * i as f32 / n;
                re += x * w.cos();
                im -= x * w.sin();
            }
            ((re * re + im * im).sqrt() * 2.0 / n, im.atan2(re))
        })
        .unzip()
}

fn peak_normalised(mut t: Vec<f32>) -> Vec<f32> {
    let peak = t.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    if peak > 1e-9 { for x in &mut t { *x /= peak; } }
    t
}

/// One table from harmonic amplitudes (sines unless phases are given).
fn from_harmonics(amps: &[f32], phases: Option<&[f32]>) -> Vec<f32> {
    let n = osc::WT_SIZE;
    let t = (0..n)
        .map(|i| {
            amps.iter().enumerate().map(|(k, a)| {
                let ph = phases.and_then(|p| p.get(k)).copied().unwrap_or(-std::f32::consts::FRAC_PI_2);
                // phase −π/2 = a sine, the analyser's convention for sin()
                a * (2.0 * std::f32::consts::PI * (k + 1) as f32 * i as f32 / n as f32 + ph).cos()
            }).sum()
        })
        .collect();
    peak_normalised(t)
}

/// Any-length samples, linearly resampled to one cycle of WT_SIZE.
fn from_samples(s: &[f32]) -> Vec<f32> {
    let n = osc::WT_SIZE;
    let len = s.len();
    let t = (0..n)
        .map(|i| {
            let x = i as f32 * len as f32 / n as f32;
            let (j, f) = (x.floor() as usize % len, x.fract());
            s[j] * (1.0 - f) + s[(j + 1) % len] * f
        })
        .collect();
    peak_normalised(t)
}

/// Numbers that stay finite as f32 and within a sane amplitude — 1e39 is inf
/// in f32, and inf / inf would put NaN in the wavetable.
fn floats(v: &Value) -> Option<Vec<f32>> {
    v.as_array()?.iter().map(|x| x.as_f64().filter(|f| f.is_finite() && f.abs() <= 1e6).map(|f| f as f32)).collect()
}

pub fn handle(shared: &SharedParamsInner, doc: &str, op: &str, value: Value) -> Result<Value, String> {
    match (doc, op) {
        ("", "list") | ("wavetable", "list") => Ok(json!([{
            "doc": "wavetable",
            "get": format!("frames[]: harmonics[1..={HARMONICS}] amplitude (peak-normalised table) and phase, plus a {PREVIEW}-point preview; WT Pos morphs between frames"),
            "set": format!("{{harmonics: [[a1, a2, ...], ...], phases?: [[...], ...]}} (sines by default) or {{frames: [[samples of one cycle, any length], ...]}}; 1..={FRAMES_MAX} frames, each peak-normalised"),
        }])),
        ("wavetable", "get") => {
            let frames: Vec<Vec<f32>> = shared.wavetable.lock().iter().map(|m| m.levels[0].to_vec()).collect();
            let out: Vec<Value> = frames.iter().map(|t| {
                let (amp, phase) = analyse(t);
                let preview: Vec<f32> = (0..PREVIEW).map(|i| t[i * t.len() / PREVIEW]).collect();
                json!({"harmonics": amp, "phases": phase, "preview": preview})
            }).collect();
            Ok(json!({"wt_size": osc::WT_SIZE, "frames": out}))
        }
        ("wavetable", "set") => {
            let tables: Vec<Vec<f32>> = if let Some(h) = value.get("harmonics").and_then(Value::as_array) {
                let ph: Vec<Option<Vec<f32>>> = value.get("phases").and_then(Value::as_array)
                    .map(|p| p.iter().map(floats).collect()).unwrap_or_default();
                h.iter().enumerate().map(|(i, a)| {
                    let a = floats(a).ok_or("harmonics: each frame is a list of numbers")?;
                    if a.is_empty() || a.len() > 512 { return Err("harmonics: 1..512 per frame".to_string()); }
                    Ok(from_harmonics(&a, ph.get(i).and_then(|p| p.as_deref())))
                }).collect::<Result<_, String>>()?
            } else if let Some(f) = value.get("frames").and_then(Value::as_array) {
                f.iter().map(|s| {
                    let s = floats(s).ok_or("frames: each frame is a list of numbers")?;
                    if s.len() < 2 { return Err("frames: at least 2 samples".to_string()); }
                    Ok(from_samples(&s))
                }).collect::<Result<_, String>>()?
            } else {
                return Err("set needs {harmonics: [[...]]} or {frames: [[...]]}".into());
            };
            if tables.is_empty() || tables.len() > FRAMES_MAX {
                return Err(format!("1..={FRAMES_MAX} frames, got {}", tables.len()));
            }
            let n = tables.len();
            push_custom_frames(shared, tables.iter().map(|t| osc::mip_from_table(t)).collect());
            shared.wavetable_gen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(json!({"frames": n}))
        }
        _ => Err(format!("Wave has one document, \"wavetable\" (list/get/set); got {doc:?} {op:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harmonics_round_trip() {
        let t = from_harmonics(&[1.0, 0.0, 0.5, 0.0, 0.25], None);
        let (amp, _) = analyse(&t);
        // peak-normalised, so compare ratios to the fundamental
        let r3 = amp[2] / amp[0];
        let r5 = amp[4] / amp[0];
        assert!((r3 - 0.5).abs() < 0.01 && (r5 - 0.25).abs() < 0.01, "3rd {r3}, 5th {r5}");
        assert!(amp[1] / amp[0] < 0.01 && amp[3] / amp[0] < 0.01, "even harmonics stay out");
    }

    #[test]
    fn a_sine_reads_as_harmonic_one() {
        let t: Vec<f32> = (0..osc::WT_SIZE).map(|i| (2.0 * std::f32::consts::PI * i as f32 / osc::WT_SIZE as f32).sin()).collect();
        let (amp, _) = analyse(&t);
        assert!((amp[0] - 1.0).abs() < 1e-3 && amp[1..].iter().all(|a| *a < 1e-3));
        assert_eq!(from_samples(&[0.0, 1.0, 0.0, -1.0]).len(), osc::WT_SIZE);
    }
}
