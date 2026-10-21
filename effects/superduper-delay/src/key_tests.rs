//! Key Duck through the real per-sample path (`stereo_process`), with the bus
//! key faked by writing the scope's key bands directly.

use super::*;

const SR: f32 = 48_000.0;

/// Two seconds of noise through the delay at Mix 1 (echoes only); returns
/// the output's RMS over the last second, once the key envelope has settled.
fn wet_rms(key_db: Option<f32>, key_duck: f32) -> f32 {
    let shared = PluginShared::new();
    if let Some(db) = key_db {
        for a in shared.scope.key_bands.iter() { a.store(db, Ordering::Relaxed); }
    }
    let mut p = PluginAudioProcessor::new(&shared, SR, 512);
    let mut seed = 1u32;
    let mut noise = || { seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223); (seed >> 8) as f32 / 8_388_608.0 - 1.0 };
    let (block, blocks) = (512, (2.0 * SR) as usize / 512);
    let (tl, tr) = (0.35 * SR, 0.38 * SR);
    let mut tail = Vec::new();
    for b in 0..blocks {
        let inp: Vec<f32> = (0..block).map(|_| 0.3 * noise()).collect();
        let (mut ol, mut or) = (vec![0.0; block], vec![0.0; block]);
        // What process() does with the key, once per block.
        p.key_duck = key_duck;
        p.key.update(&shared.scope, key_duck, 24.0, SR);
        stereo_process(
            &mut p,
            ChannelPair::InputOutput(&inp, &mut ol), Some(ChannelPair::InputOutput(&inp, &mut or)),
            SR, false, 0, tl, tr, 0.45, 6500.0, 1.0, 1.0, false, 0.0, 5.0, 200.0,
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
fn loud_key_ducks_the_echoes_by_key_duck() {
    let change = db(wet_rms(Some(-10.0), 9.0)) - db(wet_rms(None, 9.0));
    assert!((change + 9.0).abs() < 0.5, "wet moved {change:.2} dB, want −9");
}

#[test]
fn key_at_the_middle_of_the_range_ducks_half() {
    // Span level of all-equal bands is band + 10·log10(n bands in 100 Hz–8 kHz);
    // put the key at −38 dB overall = floor + half of the 24 dB range.
    let n = (0..superduper_dsp_sdk::bus::BANDS)
        .filter(|&b| (key::KEY_LO_HZ..=key::KEY_HI_HZ).contains(&(20.0 * 1000f32.powf((b as f32 + 0.5) / 32.0))))
        .count() as f32;
    let band = -38.0 - 10.0 * n.log10();
    let change = db(wet_rms(Some(band), 12.0)) - db(wet_rms(None, 12.0));
    assert!((change + 6.0).abs() < 0.5, "wet moved {change:.2} dB, want −6");
}

#[test]
fn silent_key_does_nothing() {
    assert_eq!(wet_rms(Some(-120.0), 12.0), wet_rms(None, 0.0));
}
