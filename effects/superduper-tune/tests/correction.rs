//! Autotune correction accuracy — feed a steady detuned tone and confirm the
//! output pitch snaps to the intended note (Scale / MIDI targets). PSOLA has
//! look-behind latency + a settle, so we measure the tail.

use superduper_synth_core::pitch::detect_pitch_hz;
use superduper_tune::dsp::{Tune, TuneParams, TARGET_MIDI, TARGET_SCALE};
use superduper_tune::scale;

const SR: f32 = 48_000.0;
const BLOCK: usize = 256;

/// Render `seconds` of a sine at `in_hz` through Tune. Returns the (mono)
/// output plus the correction the engine settled on (semitones) and the pitch
/// it detected on the input.
fn render(in_hz: f32, seconds: f32, p: &TuneParams) -> (Vec<f32>, f32, f32) {
    let total = (SR * seconds) as usize;
    let mut tune = Tune::new(SR, BLOCK);
    let mut out = vec![0.0f32; total];
    let mut phase = 0.0f32;
    let inc = std::f32::consts::TAU * in_hz / SR;

    let mut ol = vec![0.0f32; BLOCK];
    let mut or = vec![0.0f32; BLOCK];
    let sc = vec![0.0f32; BLOCK]; // no sidechain routed
    let mut pos = 0;
    while pos + BLOCK <= total {
        let mut il = vec![0.0f32; BLOCK];
        let mut ir = vec![0.0f32; BLOCK];
        for k in 0..BLOCK {
            let s = 0.5 * phase.sin();
            il[k] = s;
            ir[k] = s;
            phase += inc;
            if phase > std::f32::consts::TAU {
                phase -= std::f32::consts::TAU;
            }
        }
        tune.process(&il, &ir, &sc, &sc, &mut ol, &mut or, p);
        out[pos..pos + BLOCK].copy_from_slice(&ol);
        pos += BLOCK;
    }
    (out, tune.correction_st(), tune.detected_hz())
}

/// Measure output pitch on the settled tail (skip the first 0.6 s).
fn tail_hz(out: &[f32]) -> f32 {
    let skip = (SR * 0.6) as usize;
    let tail = &out[skip.min(out.len())..];
    detect_pitch_hz(tail, SR as u32).unwrap_or(0.0)
}

fn cents(a: f32, b: f32) -> f32 {
    1200.0 * (a / b).log2()
}

/// Where the correction maps the input to (Hz).
fn corrected_hz(in_hz: f32, corr_st: f32) -> f32 {
    in_hz * 2f32.powf(corr_st / 12.0)
}

#[test]
fn scale_snaps_sharp_note_down_to_scale() {
    // 460 Hz sits between A4 (440) and A#4 (466). In C major A is in the scale
    // and A# is not, so the correction must pull it down to A4 (~-0.77 st).
    let p = TuneParams {
        key: 0,                        // C
        scale_mask: scale::SCALES[1].1, // Major
        target: TARGET_SCALE,
        retune_ms: 0.0,                // hard tune
        amount: 1.0,
        ..TuneParams::default()
    };
    let (out, corr, det) = render(460.0, 2.0, &p);
    let target = corrected_hz(460.0, corr);
    eprintln!(
        "scale-snap: in 460 Hz (detected {det:.1}) → corr {corr:+.2} st → {target:.1} Hz (out tail {:.1})",
        tail_hz(&out)
    );
    assert!((corr - (-0.77)).abs() < 0.20, "expected ≈-0.77 st snap to A4, got {corr:+.2}");
    assert!((cents(target, 440.0)).abs() < 15.0, "correction should land on A4 440 Hz, lands {target:.1}");
}

#[test]
fn scale_passes_in_key_note_through() {
    // 220 Hz = A3, already in C major → correction ≈ 0.
    let p = TuneParams {
        key: 0,
        scale_mask: scale::SCALES[1].1,
        target: TARGET_SCALE,
        retune_ms: 0.0,
        amount: 1.0,
        ..TuneParams::default()
    };
    let (_out, corr, det) = render(220.0, 2.0, &p);
    eprintln!("in-key: in 220 Hz (detected {det:.1}) → corr {corr:+.2} st (expect ≈0)");
    assert!(corr.abs() < 0.20, "in-key note should not be corrected, got {corr:+.2} st");
}

#[test]
fn midi_target_pulls_to_played_note() {
    // Sing 300 Hz, hold MIDI C4 (261.63 Hz) → correction pulls to C4.
    let p = TuneParams {
        target: TARGET_MIDI,
        midi_note: 60, // C4
        retune_ms: 0.0,
        amount: 1.0,
        ..TuneParams::default()
    };
    let (out, corr, det) = render(300.0, 2.0, &p);
    let c4 = scale::midi_to_hz(60.0);
    let target = corrected_hz(300.0, corr);
    eprintln!(
        "midi-target: sing 300 Hz (detected {det:.1}), key C4 → corr {corr:+.2} st → {target:.1} Hz (out tail {:.1})",
        tail_hz(&out)
    );
    assert!((cents(target, c4)).abs() < 15.0, "correction should land on C4 {c4:.1}, lands {target:.1}");
}
