//! Key Mod through the cutoff the audio loop uses (`modulated_cutoff_hz`),
//! with the bus key faked by writing the scope's key bands directly.

use super::*;

const SR: f32 = 48_000.0;

/// Cutoff (Hz) after one second at Key Mod `oct`, the key at `key_db` per band.
fn cutoff_after_1s(key_db: Option<f32>, oct: f32) -> f32 {
    let shared = PluginShared::new();
    shared.params[P_KEY_MOD].store(oct, Ordering::Relaxed);
    if let Some(db) = key_db {
        for a in shared.scope.key_bands.iter() { a.store(db, Ordering::Relaxed); }
    }
    let mut p = PluginAudioProcessor::new(&shared, SR);
    let mut hz = 0.0;
    for _ in 0..(SR as usize / 512) {
        // What process() does with the key, once per block.
        p.key.update(&shared.scope, oct, 24.0, SR);
        p.key_mod_st = 12.0 * oct;
        for _ in 0..512 { hz = p.modulated_cutoff_hz(64.0, 0.0, SR); }
    }
    hz
}

#[test]
fn off_leaves_the_cutoff_alone_even_with_a_loud_key() {
    assert_eq!(cutoff_after_1s(Some(-10.0), 0.0), cutoff_units_to_hz(64.0));
}

#[test]
fn loud_key_moves_the_cutoff_by_key_mod_octaves() {
    let base = cutoff_units_to_hz(64.0);
    for oct in [-2.0f32, 1.5] {
        let moved = (cutoff_after_1s(Some(-10.0), oct) / base).log2();
        assert!((moved - oct).abs() < 0.02, "moved {moved:.3} oct, want {oct}");
    }
}

#[test]
fn silent_key_does_nothing() {
    assert_eq!(cutoff_after_1s(Some(-120.0), -2.0), cutoff_units_to_hz(64.0));
    assert_eq!(cutoff_after_1s(None, 3.0), cutoff_units_to_hz(64.0));
}
