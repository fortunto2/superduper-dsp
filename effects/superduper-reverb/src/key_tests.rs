//! Key Duck through the real per-sample path (`stereo_process`), with the bus
//! key faked by writing the scope's key bands directly.

use super::*;

const SR: f32 = 48_000.0;

/// Two seconds of noise through the reverb at Mix 1 (wet only); returns the
/// output's RMS over the last second, once the key envelope has settled.
fn wet_rms(key_db: Option<f32>, key_duck: f32) -> f32 {
    let scope = superduper_synth_core::gui::LiveScope::new(64);
    if let Some(db) = key_db {
        for a in scope.key_bands.iter() { a.store(db, Ordering::Relaxed); }
    }
    let mut state = Box::<PlateState>::default();
    let mut ducker = plate::Ducker::default();
    let mut sm: [SmoothedParam; 8] = [0.3, 1.0, 0.0, 0.7, 0.7, 0.4, 10.0, 0.3].map(SmoothedParam::new);
    let mut key = key::KeyEnvelope::default();
    let mut seed = 1u32;
    let mut noise = || { seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223); (seed >> 8) as f32 / 8_388_608.0 - 1.0 };
    let (block, blocks) = (512, (2.0 * SR) as usize / 512);
    let mut tail = Vec::new();
    for b in 0..blocks {
        let inp: Vec<f32> = (0..block).map(|_| 0.3 * noise()).collect();
        let (mut ol, mut or) = (vec![0.0; block], vec![0.0; block]);
        key.update(&scope, key_duck, 24.0, SR);
        let [mix, width, duck, size, decay, damp, pre, modu] = &mut sm;
        stereo_process(
            &mut state, &mut ducker, mix, width, duck, size, decay, damp, pre, modu,
            inp.as_slice(), ol.as_mut_slice(), Some((inp.as_slice(), or.as_mut_slice())),
            None, SR, 0.7, 0.7, 0.4, 10.0, 0.3, 1.0, 1.0, 0.0, 5.0, 150.0, false,
            (&mut key, key_duck),
        );
        if b >= blocks / 2 { tail.extend_from_slice(&ol); }
    }
    (tail.iter().map(|x| x * x).sum::<f32>() / tail.len() as f32).sqrt()
}

fn db(x: f32) -> f32 { 20.0 * x.max(1e-9).log10() }

#[test]
fn off_is_bit_identical_even_with_a_loud_key() {
    assert_eq!(wet_rms(None, 0.0), wet_rms(Some(-10.0), 0.0));
}

#[test]
fn loud_key_ducks_the_wet_by_key_duck() {
    let dry_key = wet_rms(None, 12.0);
    let ducked = wet_rms(Some(-10.0), 12.0);
    let change = db(ducked) - db(dry_key);
    assert!((change + 12.0).abs() < 0.5, "wet moved {change:.2} dB, want −12");
}

#[test]
fn silent_key_does_nothing() {
    assert_eq!(wet_rms(Some(-120.0), 12.0), wet_rms(None, 0.0));
}
