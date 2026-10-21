//! Key Open with no key track picked is no reaction, not silence.
//!
//! The bus key reads −120 dB when nothing is on the bus. Before 2026-10-21
//! that was "the key is quiet" (factor 0), so the "Chorus Swell" preset —
//! Key Open 1 with no key picked yet — shipped silent doubles. With no key
//! the doubles' gain is exactly 1, the same as Key Open 0, until a key is
//! picked (this in-process host has no bus key, like a fresh insert).

use clack_common::events::{event_types::ParamValueEvent, Pckn};
use clack_common::utils::{ClapId, Cookie};
use sdsp_test_kit::probes;
use sdsp_test_kit::PluginUnderTest;
use superduper_double as dbl;

struct Target;
impl PluginUnderTest for Target {
    type Plugin = dbl::SuperDuperDouble;
    const ID: &'static std::ffi::CStr = c"co.superduperai.double";
}

const SR: f64 = 48_000.0;

fn render_set(x: &[f32], set: &'static [(usize, f64)]) -> Vec<f32> {
    sdsp_test_kit::host::render_with::<Target>(SR, 512, x, x, move |pos, buf| {
        if pos == 0 {
            for &(i, v) in set {
                let id = ClapId::new(i as u32);
                buf.push(&ParamValueEvent::new(0, id, Pckn::match_all(), v, Cookie::empty()));
            }
        }
    })
    .0
}

#[test]
fn key_open_without_a_key_plays_the_doubles_as_key_open_zero_does() {
    let voice = sdsp_test_kit::signals::voiced(1.5);
    // Doubles only (Dry 0), so the dry path cannot hide silent doubles.
    let closed = render_set(&voice, &[(dbl::P_DRY, 0.0), (dbl::P_VOICES, 3.0), (dbl::P_KEY_OPEN, 0.0)]);
    let open = render_set(&voice, &[(dbl::P_DRY, 0.0), (dbl::P_VOICES, 3.0), (dbl::P_KEY_OPEN, 1.0)]);
    let (a, b) = (probes::rms_db(&closed), probes::rms_db(&open));
    println!("doubles only: Key Open 0 → {a:.1} dBFS, Key Open 1 with no key → {b:.1} dBFS");
    assert!(a > -40.0, "Key Open 0 must produce doubles ({a:.1} dBFS)");
    assert!(b > -40.0, "Key Open 1 with no key picked must not silence the doubles ({b:.1} dBFS)");
    assert!(
        closed.iter().zip(&open).all(|(x, y)| x.to_bits() == y.to_bits()),
        "with no key the gain is exactly 1: the two renders must be bit-identical"
    );
}

#[test]
fn key_target_treats_no_key_as_open_and_a_quiet_key_as_closed() {
    assert_eq!(dbl::key_target(1.0, -120.0, 24.0), 1.0, "no key: no reaction");
    assert_eq!(dbl::key_target(0.5, -119.5, 24.0), 1.0, "the bus's 'nothing' floor counts as no key");
    assert_eq!(dbl::key_target(1.0, -80.0, 24.0), 0.0, "a key that is quiet closes the doubles");
    assert_eq!(dbl::key_target(1.0, -10.0, 24.0), 1.0, "a loud key opens them");
    assert!((dbl::key_target(1.0, -38.0, 24.0) - 0.5).abs() < 1e-6, "halfway up Key Range");
    assert_eq!(dbl::key_target(0.0, -10.0, 24.0), 0.0, "Key Open 0 glides to exactly no reaction");
}

#[test]
fn chorus_swell_preset_is_not_silent_without_a_key() {
    let p = dbl::presets::PRESETS.iter().find(|p| p.name == "Chorus Swell").expect("preset exists");
    let set: Vec<(usize, f64)> = p.values.iter().enumerate().map(|(i, v)| (i, *v as f64)).chain([(dbl::P_DRY, 0.0)]).collect();
    let set: &'static [(usize, f64)] = Box::leak(set.into_boxed_slice());
    let voice = sdsp_test_kit::signals::voiced(1.5);
    let out = render_set(&voice, set);
    let level = probes::rms_db(&out);
    println!("Chorus Swell, doubles only, no key: {level:.1} dBFS");
    assert!(level > -40.0, "Chorus Swell with no key picked plays silent doubles ({level:.1} dBFS)");
}
