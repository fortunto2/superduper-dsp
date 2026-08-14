//! ASCII spectra for SuperDuper Formant — the human/AI-readable assertion.
//!
//! Run: `cargo test --release -p superduper-formant --test spectrum -- --nocapture`
//!
//! What to look for: each vowel's chart should show its own peak pattern, and
//! the /i/ chart should look clearly different from /u/ (bright vs dark). The
//! Follow chart should match the vowel that was sung into the sidechain, not the
//! pad default.

use superduper_formant::dsp::{FmtParams, FormantFx, MODE_FOLLOW};
use superduper_synth_core::analysis::{ascii_spectrum, spectrum_with_freq, AsciiSpectrumOpts};
use superduper_synth_core::formant::{Formant, FORMANT_PRESETS};

const SR: f32 = 48_000.0;

fn pulse_train(f0: f32, n: usize, amp: f32) -> Vec<f32> {
    let kmax = ((SR * 0.45).min(5_000.0) / f0) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let mut s = 0.0;
            for k in 1..=kmax {
                s += (std::f32::consts::TAU * f0 * k as f32 * t).sin() / k as f32;
            }
            s * amp
        })
        .collect()
}

fn run(fx: &mut FormantFx, input: &[f32], sc: &[f32], p: &FmtParams) -> Vec<f32> {
    let mut l = vec![0.0f32; input.len()];
    let mut r = vec![0.0f32; input.len()];
    let sc = if sc.is_empty() { vec![0.0; input.len()] } else { sc.to_vec() };
    const BLOCK: usize = 512;
    let mut pos = 0;
    while pos < input.len() {
        let end = (pos + BLOCK).min(input.len());
        fx.process_stereo(
            &input[pos..end],
            &input[pos..end],
            &mut l[pos..end],
            &mut r[pos..end],
            &sc[pos..end],
            &sc[pos..end],
            p,
        );
        pos = end;
    }
    l
}

fn show(title: &str, samples: &[f32]) {
    let tail = &samples[samples.len().saturating_sub(8192)..];
    let spec = spectrum_with_freq(tail, SR);
    println!("\n=== {title} ===");
    println!("{}", ascii_spectrum(&spec, &AsciiSpectrumOpts::default()));
}

#[test]
fn vowel_spectra() {
    let src = pulse_train(120.0, SR as usize / 2, 0.3);
    show("dry pulse train (120 Hz)", &src);

    // Every vowel from the shared table, plus the Bashkir kubyz setting.
    for idx in [1usize, 3, 5, 6] {
        let v = FORMANT_PRESETS[idx];
        let mut fx = FormantFx::new(SR);
        let p = FmtParams {
            f1: v.f[0],
            f2: v.f[1],
            f3: v.f[2],
            mix: 1.0,
            ..FmtParams::default()
        };
        let out = run(&mut fx, &src, &[], &p);
        show(
            &format!("{} — F {:.0}/{:.0}/{:.0} Hz", v.name, v.f[0], v.f[1], v.f[2]),
            &out,
        );
    }
}

#[test]
fn follow_mode_spectrum() {
    let vowel = FORMANT_PRESETS[3]; // /i/
    let n = SR as usize;
    let drone = pulse_train(100.0, n, 0.3);
    let raw = pulse_train(150.0, n, 0.3);
    let mut vf = Formant::default();
    let voice: Vec<f32> = raw
        .iter()
        .map(|&s| vf.process(s, s, SR, vowel.f, vowel.bw, vowel.gain, 1.0).0)
        .collect();

    show("the sung voice (/i/)", &voice);
    show("the bare drone (100 Hz)", &drone);

    let mut fx = FormantFx::new(SR);
    let p = FmtParams {
        mode: MODE_FOLLOW,
        follow: 1.0,
        glide_ms: 20.0,
        mix: 1.0,
        ..FmtParams::default()
    };
    let out = run(&mut fx, &drone, &voice, &p);
    let t = fx.tracked_formants();
    show(
        &format!("drone articulated by the voice — tracked {:.0}/{:.0}/{:.0} Hz", t[0], t[1], t[2]),
        &out,
    );
}
