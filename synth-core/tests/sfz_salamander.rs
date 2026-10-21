//! Known-answer test against sfizz on the real Salamander Grand V3.
//!
//! The reference numbers were measured 2026-10-20 by rendering the same
//! notes through `sfizz_render` 1.2.3 (RMS over the 2 s a key is held):
//! C4 at velocity 20 / 60 / 100 / 127 = −59.0 / −44.8 / −35.4 / −31.2 dB.
//! Ours ran a constant +7.4 dB hotter on every note — sfizz's −7.35 dB
//! default master volume — with waveform correlation 0.994, so only the
//! *shape* is asserted here: velocity layers, `amp_veltrack` via CC99,
//! `amplitude_oncc7` all line up when the per-velocity differences match.
//!
//! Needs the pack at ~/Music/SFZ/SalamanderGrandPiano (not in git). Without
//! it the test says so and fails only when SDSP_REQUIRE_PACKS=1.
#![cfg(feature = "sfz-load")]

use superduper_synth_core::sfz::{load::load_instrument, Engine, DEFAULT_VOICES};

fn rms_db(x: &[f32]) -> f64 {
    let e: f64 = x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len().max(1) as f64;
    10.0 * (e + 1e-20).log10()
}

#[test]
fn salamander_velocity_curve_matches_sfizz() {
    let sfz = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default()
        .join("Music/SFZ/SalamanderGrandPiano/Salamander Grand Piano V3.sfz");
    if !sfz.exists() {
        eprintln!("SKIP: {} not installed", sfz.display());
        assert!(std::env::var("SDSP_REQUIRE_PACKS").is_err(), "pack required but missing");
        return;
    }
    let inst = load_instrument(&sfz, 8, None).expect("load");
    assert!(inst.missing.is_empty(), "{:?}", &inst.missing[..inst.missing.len().min(3)]);
    assert!(inst.def.unsupported.is_empty(), "{:?}", inst.def.unsupported);

    let sr = 48_000usize;
    let mut levels = Vec::new();
    for vel in [20u8, 60, 100, 127] {
        let mut e = Engine::new(sr as f32, DEFAULT_VOICES);
        e.reset(&inst);
        e.note_on(&inst, 60, vel as f32 / 127.0);
        let (mut l, mut r) = (vec![0f32; 2 * sr], vec![0f32; 2 * sr]);
        for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) { e.render(&inst, cl, cr); }
        let both: Vec<f32> = l.iter().zip(&r).flat_map(|(a, b)| [*a, *b]).collect();
        levels.push(rms_db(&both));
    }
    let sfizz = [-59.0, -44.8, -35.4, -31.2];
    let offset = levels[3] - sfizz[3];
    eprintln!("ours {levels:?}, offset vs sfizz {offset:.2} dB");
    assert!((offset - 7.35).abs() < 0.3, "master offset drifted: {offset:.2}");
    for i in 0..4 {
        let d = levels[i] - sfizz[i] - offset;
        assert!(d.abs() < 0.3, "velocity {} off by {d:.2} dB relative to sfizz", [20, 60, 100, 127][i]);
    }
}
