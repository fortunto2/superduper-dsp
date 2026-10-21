//! Property tests: what must hold for *any* input, not just the one in a
//! unit test. The review of 2026-10-21 found two NaN paths that a random
//! input would have hit in a second (inf through a formant bank, inf through
//! a wavetable peak-normaliser); these pin the shared blocks so the next one
//! is caught before it ships.
//!
//! `cargo test --release -p superduper-synth-core --test invariants`

use proptest::prelude::*;
use superduper_synth_core::bus_key::{duck_gain, key_factor, span_level_db};
use superduper_synth_core::drum_voices::OnePoleBp;
use superduper_synth_core::dsp_blocks::{inflate, tanh_drive, tape_clip, tube_clip, Biquad};
use superduper_synth_core::mixmap::{sum_deficit_db, MaskingTracker, TrackSpectrum, BANDS};

/// Any f32, including NaN, ±inf and subnormals.
fn any_f32() -> impl Strategy<Value = f32> {
    prop_oneof![
        any::<f32>(),
        Just(f32::NAN),
        Just(f32::INFINITY),
        Just(f32::NEG_INFINITY),
        Just(f32::MIN_POSITIVE / 8.0),
        (-2.0f32..2.0),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// The waveshapers never produce NaN or inf from a finite input, stay
    /// inside ±1 where they promise to, and are odd (no DC from symmetric input).
    #[test]
    fn waveshapers_are_bounded_and_odd(x in -4.0f32..4.0, drive in 0.0f32..64.0, curve in -50.0f32..50.0) {
        for (name, y, ny) in [
            ("inflate", inflate(x, curve), inflate(-x, curve)),
            ("tanh", tanh_drive(x, drive), tanh_drive(-x, drive)),
            ("tape", tape_clip(x, drive), tape_clip(-x, drive)),
        ] {
            prop_assert!(y.is_finite(), "{name}({x}, …) = {y}");
            prop_assert!((y + ny).abs() < 1e-3, "{name} not odd: f({x}) = {y}, f(-{x}) = {ny}");
        }
        // Tube is asymmetric on purpose (even harmonics) and leaves a small
        // DC that the Saturator's DC blocker removes; it must stay finite and bounded.
        let t = tube_clip(x, drive);
        prop_assert!(t.is_finite() && t.abs() <= 2.0, "tube({x}, {drive}) = {t}");
        prop_assert!(tube_clip(0.0, drive).abs() < 0.02, "tube DC at silence {}", tube_clip(0.0, drive));
        prop_assert!(inflate(x, curve).abs() <= 1.0 + 1e-6);
        prop_assert!(tanh_drive(x, drive).abs() <= 1.0 + 1e-6);
    }

    /// A biquad fed finite audio never blows up: for every RBJ shape over
    /// its whole parameter range, 512 samples of ±1 input stay finite and
    /// under +60 dB (a resonance may ring, it may not diverge).
    #[test]
    fn biquads_stay_finite(shape in 0u8..7, hz in 10.0f32..24_000.0, q in 0.05f32..30.0, db in -30.0f32..30.0, seed in any::<u64>(), sr in 1000.0f32..192_000.0) {
        let mut b = Biquad::default();
        match shape {
            0 => b.set_peaking(sr, hz, q, db),
            1 => b.set_low_shelf(sr, hz, q.min(2.0), db),
            2 => b.set_high_shelf(sr, hz, q.min(2.0), db),
            3 => b.set_hpf(sr, hz, q),
            4 => b.set_lpf(sr, hz, q),
            5 => b.set_bandpass(sr, hz, q),
            _ => b.set_notch(sr, hz, q),
        }
        let mut s = seed | 1;
        let mut peak = 0.0f32;
        for _ in 0..512 {
            s ^= s << 13; s ^= s >> 7; s ^= s << 17;
            let x = (s as f32 / u64::MAX as f32) * 2.0 - 1.0;
            let y = b.process(x);
            prop_assert!(y.is_finite(), "shape {shape} hz {hz} q {q} db {db}: {y}");
            peak = peak.max(y.abs());
        }
        prop_assert!(peak < 1000.0, "shape {shape} hz {hz} q {q} db {db}: peak {peak} (+{:.0} dB)", 20.0 * peak.log10());
    }

    /// The drum voices' own band-pass stays finite at any host rate: an
    /// 8 kHz hat band at a 1234 Hz rate went to -inf (clap-validator).
    #[test]
    fn drum_bandpass_stays_finite(sr in 1000.0f32..192_000.0, hz in 20.0f32..20_000.0, q in 0.05f32..30.0, seed in any::<u64>()) {
        let mut f = OnePoleBp::default();
        f.set(sr, hz, q);
        let mut s = seed | 1;
        for _ in 0..512 {
            s ^= s << 13; s ^= s >> 7; s ^= s << 17;
            let x = (s as f32 / u64::MAX as f32) * 2.0 - 1.0;
            let y = f.process(x);
            prop_assert!(y.is_finite() && y.abs() < 1000.0, "sr {sr} hz {hz} q {q}: {y}");
        }
    }

    /// The bus key maths is total: any level in, a finite factor in 0..=1
    /// and a gain in 0..=1 out — a NaN key must not freeze a plugin.
    #[test]
    fn bus_key_is_total(key_db in any_f32(), range in any_f32(), depth in any_f32(), f in any_f32()) {
        let kf = key_factor(key_db, range);
        prop_assert!((0.0..=1.0).contains(&kf) || kf.is_nan() == false, "factor {kf}");
        prop_assert!(kf.is_finite() && (0.0..=1.0).contains(&kf), "key_factor({key_db}, {range}) = {kf}");
        let g = duck_gain(f, depth);
        prop_assert!(g.is_finite() && (0.0..=1.0).contains(&g), "duck_gain({f}, {depth}) = {g}");
        let bands = [key_db; BANDS];
        let lvl = span_level_db(&bands, 100.0, 8000.0);
        prop_assert!(lvl.is_finite() && lvl >= -120.0, "span_level_db = {lvl}");
    }

    /// Masking shares are shares: in 0..=1, finite, for any band levels.
    #[test]
    fn masking_shares_are_shares(a in prop::collection::vec(-130.0f32..0.0, BANDS), b in prop::collection::vec(-130.0f32..0.0, BANDS), frames in 1usize..40) {
        let t = |name: &str, v: &[f32]| TrackSpectrum { name: name.into(), bus: false, rms_db: -20.0, bands_db: std::array::from_fn(|i| v[i]) };
        let mut m = MaskingTracker::mean();
        for _ in 0..frames { m.update(&[t("A", &a), t("B", &b)]); }
        for c in m.top(8) {
            prop_assert!(c.share.is_finite() && (0.0..=1.0 + 1e-6).contains(&c.share), "share {}", c.share);
        }
        for c in m.top_buried(8, None) {
            prop_assert!(c.share.is_finite() && (0.0..=1.0 + 1e-6).contains(&c.share), "buried {}", c.share);
        }
        let heat = m.band_heat(None);
        prop_assert!(heat.iter().all(|h| h.is_finite() && *h >= 0.0 && *h <= 1.0 + 1e-6));
    }

    /// sum_deficit_db removes any broadband offset exactly and never NaNs.
    #[test]
    fn sum_deficit_removes_broadband_gain(p in prop::collection::vec(1e-9f64..1.0, BANDS), gain_db in -40.0f64..6.0) {
        let tp: [f64; BANDS] = std::array::from_fn(|i| p[i]);
        let mp: [f64; BANDS] = std::array::from_fn(|i| p[i] * 10f64.powf(gain_db / 10.0));
        let d = sum_deficit_db(&tp, &mp);
        prop_assert!(d.iter().all(|x| x.is_finite() && x.abs() < 1e-3), "{d:?}");
    }
}
