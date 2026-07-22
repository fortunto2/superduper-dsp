//! Voice-mode click audit — the user reported audible clicks in PSOLA mode,
//! and a regression at exactly −12 st (octave down, the α=0.5 degenerate case
//! where 2·T0 grains just touch). Drive a realistic (glottal-pulse) voice
//! through Voice mode across the shift range and assert no sample-to-sample
//! discontinuities (grain-boundary clicks) anywhere.
//!
//! Run: `cargo test -p superduper-pitch --test click_audit -- --nocapture`

use superduper_pitch::dsp::{PitchParams, PitchShifter};
use superduper_synth_core::dsp_blocks::Biquad;

const SR: f32 = 48_000.0;

/// Source-filter voice: glottal impulse train through two formant resonators —
/// sharp epochs (like real speech), band-limited by the formants so the clean
/// signal has a modest sample-to-sample delta.
fn voice(f0: f32, n: usize) -> Vec<f32> {
    let period = SR / f0;
    let mut b1 = Biquad::default();
    b1.set_bandpass(SR, 700.0, 5.0);
    let mut b2 = Biquad::default();
    b2.set_bandpass(SR, 1800.0, 8.0);
    let mut ph = 0.0f32;
    let mut out = vec![0.0f32; n];
    for o in out.iter_mut() {
        ph += 1.0;
        let src = if ph >= period {
            ph -= period;
            1.0
        } else {
            0.0
        };
        *o = (b1.process(src) + 0.5 * b2.process(src)) * 0.6;
    }
    out
}

fn run(m: &[f32], pitch: f32) -> Vec<f32> {
    let n = m.len();
    let mut out = vec![0.0f32; n];
    let mut sh = PitchShifter::new(SR, 512);
    let p = PitchParams { pitch_st: pitch, formant_st: 0.0, mix: 1.0, output_lin: 1.0, bypassed: false };
    let mut i = 0;
    while i < n {
        let end = (i + 512).min(n);
        let inb = &m[i..end];
        let mut ol = vec![0.0f32; end - i];
        let mut orr = vec![0.0f32; end - i];
        sh.process(inb, inb, &mut ol, &mut orr, &p);
        out[i..end].copy_from_slice(&ol);
        i = end;
    }
    out
}

#[test]
fn voice_mode_no_clicks_across_range() {
    // Include the previously-broken −24 / −12 (octave-down degenerate cases).
    let m = voice(130.0, (SR * 2.0) as usize);
    for pitch in [-24.0f32, -12.0, -9.0, -6.0, -5.0, -3.0, 0.0, 5.0, 12.0] {
        let out = run(&m, pitch);
        assert!(out.iter().all(|v| v.is_finite()), "pitch {pitch}: NaN/Inf");
        let tail = &out[SR as usize / 2..];
        let max_jump = tail.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
        println!("pitch {pitch:+5}: max |x[n+1]-x[n]| = {max_jump:.4}");
        assert!(
            max_jump < 0.15,
            "audible click at pitch {pitch}: max sample jump {max_jump:.4}"
        );
    }
}
