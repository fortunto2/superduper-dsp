//! Bus-keyed dynamic Mid band.
//!
//! The first test pins that the plugin with Dyn Gain at 0 renders bit for bit
//! what it rendered before the dynamic band existed (hash recorded from the
//! pre-change build), with every static band doing something.

use clack_common::events::event_types::ParamValueEvent;
use clack_common::events::Pckn;
use clack_common::utils::{ClapId, Cookie};
use sdsp_test_kit::host::render_with;
use sdsp_test_kit::{probes, PluginUnderTest};

struct Target;
impl PluginUnderTest for Target {
    type Plugin = superduper_eq::SuperDuperEq;
    const ID: &'static std::ffi::CStr = c"co.superduperai.eq";
}

const SR: f64 = 48_000.0;

fn fnv(xs: &[f32]) -> u64 {
    xs.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, x| (h ^ x.to_bits() as u64).wrapping_mul(0x100_0000_01b3))
}

#[test]
fn dyn_off_is_bit_identical_to_the_static_eq() {
    // Low −3 dB @120, Mid +6 dB @1 kHz Q 1.5, High +4 dB, HP 60, LP 14k, Out +2.
    let set: &[(u32, f64)] = &[(1, -3.0), (2, 1000.0), (3, 6.0), (4, 1.5), (6, 4.0), (7, 60.0), (8, 14_000.0), (9, 2.0)];
    let noise = probes::noise(1 << 15, 0.3);
    let (l, r) = render_with::<Target>(SR, 512, &noise, &noise, |pos, buf| {
        if pos == 0 {
            for &(id, v) in set {
                buf.push(&ParamValueEvent::new(0, ClapId::new(id), Pckn::new(0u16, 0u16, 0u16, 0u32), v, Cookie::empty()));
            }
        }
    });
    let h = fnv(&l) ^ fnv(&r).rotate_left(1);
    eprintln!("hash {h:#018x}");
    // The events must have landed, or this pins a flat EQ.
    let (fl, _) = render_with::<Target>(SR, 512, &noise, &noise, |_, _| {});
    assert_ne!(fnv(&fl), fnv(&l), "param events were ignored");
    // The baseline was recorded on macOS arm64. MSVC's libm rounds sin/exp/pow
    // differently in the last bits, so the Windows release job failed here on
    // a hash, not on a behaviour (v0.16.0, 2026-10-21). Bit-identity is a
    // same-platform promise; the behaviour tests below run everywhere.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    assert_eq!(h, BASELINE, "Dyn Gain 0 must not change a single sample");
}

#[cfg_attr(not(all(target_os = "macos", target_arch = "aarch64")), allow(dead_code))]
const BASELINE: u64 = 0x71e68b74c2b0ba41;

// ---------------------------------------------------------------------------
// Behaviour, driven straight through EqDsp with a hand-set bus key.
// ---------------------------------------------------------------------------

use std::sync::atomic::Ordering;
use superduper_dsp_sdk::bus::{band_edges, BANDS};
use superduper_eq::{EqDsp, PluginShared, P_DYN_GAIN, P_DYN_SOURCE, P_MID_FREQ};

const SRF: f32 = 48_000.0;

/// Key track: `db` in every band whose centre is within an octave of `hz`,
/// silence elsewhere.
fn key_loud_near(shared: &PluginShared, hz: Option<f32>, db: f32) {
    let e = band_edges();
    for b in 0..BANDS {
        let c = (e[b] * e[b + 1]).sqrt();
        let near = hz.is_some_and(|h| (c / h).log2().abs() < 1.0);
        shared.scope.key_bands[b].store(if near { db } else { -120.0 }, Ordering::Relaxed);
    }
}

/// Render 1 s of a tone at `hz` (amp 0.5) in 512-frame blocks and return the
/// RMS (dB) of the last 0.25 s, after the dynamics have settled.
fn settled_tone_db(shared: &PluginShared, hz: f32, amp: f32) -> f64 {
    let n = SRF as usize;
    let x = probes::tone(SRF, hz, amp, n);
    let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
    let mut dsp = EqDsp::new(SRF, 0.0);
    for ((xi, lo), ro) in x.chunks(512).zip(l.chunks_mut(512)).zip(r.chunks_mut(512)) {
        dsp.process(shared, (xi, lo), Some((xi, ro)));
    }
    assert_eq!(l, r, "both channels share the moving bell");
    probes::rms_db(&l[n * 3 / 4..]) - probes::rms_db(&x[n * 3 / 4..])
}

fn eq(dyn_gain: f32) -> PluginShared {
    let s = PluginShared::new();
    s.params[P_MID_FREQ].store(1000.0, Ordering::Relaxed);
    s.params[P_DYN_GAIN].store(dyn_gain, Ordering::Relaxed);
    s
}

#[test]
fn a_loud_key_at_the_mid_frequency_cuts_the_tone_by_dyn_gain() {
    let s = eq(-12.0);
    key_loud_near(&s, Some(1000.0), -10.0); // well above −50 + 24
    let g = settled_tone_db(&s, 1000.0, 0.5);
    eprintln!("key loud at 1 kHz: {g:.2} dB");
    assert!((g + 12.0).abs() < 0.3, "{g:.2} dB, expected ≈ −12");
    assert!((s.dyn_now.load(Ordering::Relaxed) + 12.0).abs() < 0.05, "GUI sees the offset");
}

#[test]
fn half_way_up_the_range_cuts_half() {
    let s = eq(-12.0);
    key_loud_near(&s, Some(1000.0), -38.0);
    let g = settled_tone_db(&s, 1000.0, 0.5);
    assert!((g + 6.0).abs() < 0.3, "{g:.2} dB, expected ≈ −6");
}

#[test]
fn a_silent_key_changes_nothing() {
    let s = eq(-12.0);
    key_loud_near(&s, None, -120.0);
    let g = settled_tone_db(&s, 1000.0, 0.5);
    assert!(g.abs() < 0.05, "{g:.2} dB");
    assert_eq!(s.dyn_now.load(Ordering::Relaxed), 0.0);
}

#[test]
fn a_key_loud_far_from_the_mid_frequency_changes_nothing_there() {
    let s = eq(-12.0);
    key_loud_near(&s, Some(100.0), -6.0);
    let g = settled_tone_db(&s, 1000.0, 0.5);
    assert!(g.abs() < 0.05, "{g:.2} dB");
}

#[test]
fn dyn_gain_zero_ignores_even_a_loud_key() {
    let s = eq(0.0);
    key_loud_near(&s, Some(1000.0), -6.0);
    assert!(settled_tone_db(&s, 1000.0, 0.5).abs() < 0.05);
}

#[test]
fn self_source_cuts_a_loud_band_and_leaves_a_quiet_one() {
    let s = eq(-12.0);
    s.params[P_DYN_SOURCE].store(1.0, Ordering::Relaxed);
    key_loud_near(&s, None, -120.0); // no key at all: self mode does not need one
    let loud = settled_tone_db(&s, 1000.0, 0.5); // −9 dB RMS, past −26
    let quiet = settled_tone_db(&s, 1000.0, 0.001); // −63 dB RMS, under −50
    eprintln!("self: loud {loud:.2} dB, quiet {quiet:.2} dB");
    assert!((loud + 12.0).abs() < 0.5, "loud {loud:.2}");
    assert!(quiet.abs() < 0.05, "quiet {quiet:.2}");
}
