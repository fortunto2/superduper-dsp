//! Quality snapshot for the saturator.
//!
//! The existing `quality_audit.rs` asserts a range ("THD between −60 and −10")
//! and drives the DSP function directly. This one goes through the real CLAP
//! plugin and records exact numbers, so a change that keeps THD inside the
//! range but moves it by 4 dB — the kind that made a mix darker for weeks
//! before anyone noticed — fails instead of passing.

use sdsp_test_kit::probes::{self, db};
use sdsp_test_kit::{render_effect, PluginUnderTest, Suite};

struct Saturator;
impl PluginUnderTest for Saturator {
    type Plugin = superduper_saturator::SuperDuperSaturator;
    const ID: &'static std::ffi::CStr = c"co.superduperai.saturator";
}

const SR: f64 = 48_000.0;

#[test]
fn quality_snapshot() {
    let mut s = Suite::new(
        "saturator",
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/quality.snap"),
    );

    let frames = 1 << 15;

    // 1 kHz tone: level, harmonic content, and the band shape it produces.
    let tone = probes::tone(SR as f32, 1000.0, 0.5, frames);
    let (out_l, _) = render_effect::<Saturator>(SR, &tone, &tone);
    s.record("tone_gain_db", probes::gain_db(&tone, &out_l));
    s.record("tone_thd_db", probes::thd_at(&out_l, 1000.0, SR as f32));
    s.record("tone_peak_db", db(probes::peak(&out_l)));
    for (band, level) in probes::band_shape(&out_l, SR as f32) {
        s.record(format!("tone_band_{band}"), level);
    }

    // A near-Nyquist tone folds its harmonics back into the audible range, so
    // this is where an oversampling regression shows up.
    let hot = probes::tone(SR as f32, 18_000.0, 0.7, frames);
    let (alias_out, _) = render_effect::<Saturator>(SR, &hot, &hot);
    s.record("aliasing_18k_db", probes::aliasing_at(&alias_out, 18_000.0, SR as f32));

    // Noise: broadband level change, plus the largest sample step — a jump here
    // means the plugin started clicking.
    let noise = probes::noise(frames, 0.3);
    let (noise_out, _) = render_effect::<Saturator>(SR, &noise, &noise);
    s.record("noise_gain_db", probes::gain_db(&noise, &noise_out));
    s.record("noise_max_step", probes::max_step(&noise_out));

    sdsp_test_kit::params::record_table(&mut s, superduper_saturator::PARAMS);
    s.finish();
}

#[test]
fn parameter_table_is_consistent() {
    // Ids dense and ordered, defaults inside range, names unique, stepped
    // params actually discrete. The table is hand-maintained and its indices
    // are duplicated into P_* constants, presets and the GUI.
    sdsp_test_kit::params::check_table("superduper-saturator", superduper_saturator::PARAMS, &[]);
}

#[global_allocator]
static ALLOC: sdsp_test_kit::alloc::CountingAllocator = sdsp_test_kit::alloc::CountingAllocator;

#[test]
fn process_does_not_allocate() {
    // The real version of sdk/tests/rt_safety.rs: that one reads the text of
    // process(), this one counts every malloc inside it, helpers included —
    // which is where the ones found in review actually lived.
    let frames = 1 << 14;
    let noise = probes::noise(frames, 0.3);
    sdsp_test_kit::alloc::reset();
    let _ = render_effect::<Saturator>(SR, &noise, &noise);
    sdsp_test_kit::alloc::assert_rt_clean("superduper-saturator", frames / 512);
}
