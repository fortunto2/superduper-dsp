//! Inflate's input gain is Drive − 6 dB, so the default Drive is unity.
//!
//! Inflate is a loudness curve for a master: full scale stays full scale and
//! everything under it is lifted. Before 2026-10-21 it inherited Drive's
//! +6 dB default as its input gain, so a master peaking at −3 dBFS was
//! pushed to +3 dBFS into the curve, which clamps at 1.0 — the peaks came
//! out flat-topped. The Classic curves keep Drive as it is.

use clack_common::events::{event_types::ParamValueEvent, Pckn};
use clack_common::utils::{ClapId, Cookie};
use sdsp_test_kit::probes::{self, db};
use sdsp_test_kit::PluginUnderTest;
use superduper_saturator as sat;

struct Saturator;
impl PluginUnderTest for Saturator {
    type Plugin = sat::SuperDuperSaturator;
    const ID: &'static std::ffi::CStr = c"co.superduperai.saturator";
}

const SR: f64 = 48_000.0;

/// Render with some params set at the first block.
fn render_set(x: &[f32], set: &'static [(usize, f64)]) -> Vec<f32> {
    sdsp_test_kit::host::render_with::<Saturator>(SR, 512, x, x, move |pos, buf| {
        if pos == 0 {
            for &(i, v) in set {
                let id = ClapId::new(i as u32);
                buf.push(&ParamValueEvent::new(0, id, Pckn::match_all(), v, Cookie::empty()));
            }
        }
    })
    .0
}

/// The curve alone at unity input gain, Curve 0: what the plugin must match.
fn inflate_peak_db(amp: f32) -> f64 {
    db(superduper_synth_core::dsp_blocks::inflate(amp, 0.0))
}

#[test]
fn inflate_at_default_drive_keeps_a_minus_3_dbfs_master_unclipped() {
    let frames = 1 << 15;
    let amp = 10f32.powf(-3.0 / 20.0);
    let tone = probes::tone(SR as f32, 1000.0, amp, frames);
    // Type 3 = Inflate, Drive and Curve at their defaults (6 dB, 0).
    let out = render_set(&tone, &[(sat::P_TYPE, 3.0)]);
    let tail = &out[frames / 2..];
    let peak_db = db(probes::peak(tail));
    let flat = tail.iter().filter(|v| v.abs() >= 0.98).count();
    let thd = probes::thd_at(tail, 1000.0, SR as f32);
    let want = inflate_peak_db(amp);
    println!("Inflate, −3 dBFS in: peak {peak_db:.2} dB (curve alone {want:.2}), {flat} samples ≥ 0.98, THD {thd:.1} dB");
    assert!((peak_db - want).abs() < 0.3, "peak {peak_db:.2} dB, the curve at unity gives {want:.2}");
    assert_eq!(flat, 0, "{flat} flat-topped samples: the peaks were pushed into the curve's clamp");
}

#[test]
fn inflate_drive_knob_is_offset_by_6_db_only_for_inflate() {
    // Drive 12 on Inflate = +6 dB into the curve; a −12 dBFS tone comes out
    // where the curve puts a −6 dBFS one. Tape at Drive 6 is untouched by
    // this change (its snapshot pins it too).
    let frames = 1 << 14;
    let tone = probes::tone(SR as f32, 1000.0, 10f32.powf(-12.0 / 20.0), frames);
    let out = render_set(&tone, &[(sat::P_TYPE, 3.0), (sat::P_DRIVE, 12.0)]);
    let peak_db = db(probes::peak(&out[frames / 2..]));
    let want = inflate_peak_db(10f32.powf(-6.0 / 20.0));
    println!("Inflate, Drive 12, −12 dBFS in: peak {peak_db:.2} dB, curve at −6 dBFS {want:.2}");
    assert!((peak_db - want).abs() < 0.3, "peak {peak_db:.2} vs {want:.2}");
}
