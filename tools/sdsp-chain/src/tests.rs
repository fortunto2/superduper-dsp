//! Denoise in the headless chain: it renders, it cleans, and its 2048-sample
//! look-ahead is compensated so the output lines up with the input.

use super::*;

const SR: f64 = 48_000.0;

fn noisy_bursts(seconds: f64) -> (Audio, Vec<bool>) {
    let n = (seconds * SR) as usize;
    let mut seed = 0x1234_5678u32;
    let (mut l, mut on) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for i in 0..n {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let noise = (seed as f32 / u32::MAX as f32 * 2.0 - 1.0) * 0.03;
        // 1 s of noise alone (room tone to learn from), then 250 ms of a
        // voiced tone and 250 ms of noise, alternating.
        let voiced = i >= 48_000 && ((i - 48_000) / 12_000) % 2 == 0;
        let tone = if voiced {
            0.3 * (i as f32 * 220.0 * std::f32::consts::TAU / SR as f32).sin()
        } else {
            0.0
        };
        l.push(tone + noise);
        on.push(voiced);
    }
    (
        Audio {
            r: l.clone(),
            l,
            sr: SR,
        },
        on,
    )
}

fn spec(key: &str) -> &'static PluginSpec {
    registry()
        .iter()
        .find(|p| p.key == key)
        .unwrap_or_else(|| panic!("no `{key}` in the chain"))
}

#[test]
fn denoise_is_in_the_chain_and_lowers_the_noise_between_bursts() {
    let (input, voiced) = noisy_bursts(3.0);
    // Learn offline is automation: on over the room tone, off before speech.
    let learn = vec![(6, vec![(0.0, 1.0), (0.9, 1.0), (0.95, 0.0)])];
    let out = (spec("denoise").render)(
        &vec![(0, 0.0), (1, 20.0)],
        &learn,
        StageIo {
            input: &input,
            sidechain: None,
        },
    );
    assert_eq!(out.frames(), input.frames());
    // Score after the room tone: the passage the profile was learned for.
    let gap_energy = |a: &Audio| -> f64 {
        (48_000..a.frames())
            .filter(|&i| !voiced[i])
            .map(|i| (a.l[i] as f64).powi(2))
            .sum()
    };
    let drop_db = 10.0 * (gap_energy(&out) / gap_energy(&input)).log10();
    assert!(
        drop_db < -10.0,
        "noise between bursts only moved {drop_db:.1} dB"
    );
}

#[test]
fn a_dry_denoise_comes_out_sample_aligned_with_its_input() {
    let (input, _) = noisy_bursts(1.0);
    // Mix 0: the plugin outputs its latency-matched dry path, so a compensated
    // chain must hand back the input itself, not the input 2048 samples late.
    let out = (spec("denoise").render)(
        &vec![(8, 0.0)],
        &Vec::new(),
        StageIo {
            input: &input,
            sidechain: None,
        },
    );
    let worst = input
        .l
        .iter()
        .zip(&out.l)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        worst < 1e-5,
        "dry output differs from the input by up to {worst}"
    );
}
