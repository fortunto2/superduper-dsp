//! Pre-baked Kubyz presets — harmonic content + formants + envelope.
//!
//! The numbers come straight from the KubizBeat sample analysis JSONs:
//!   * `bashkir_reference.json`  — typical Bashkir kubyz tone
//!   * `kubyz_sample_target.json` — Yakut khomus sample target
//!
//! A preset declares 16 harmonic relative-amplitudes (linear, not dB —
//! converted from the JSON's dB at compile time), three formant frequencies
//! + bandwidths + per-band gains, and a default envelope.

use superduper_synth_core::formant::FormantPreset;

pub const N_HARMONICS: usize = 16;

#[derive(Clone, Copy)]
pub struct KubyzPreset {
    pub name: &'static str,
    /// Linear amplitudes for harmonics 1..=N. Normalised so harmonic 1 = 1.0.
    pub harmonics: [f32; N_HARMONICS],
    pub formant: FormantPreset,
    pub attack_s: f32,
    pub decay_s: f32,
    pub sustain: f32,
    pub release_s: f32,
    /// Velocity-coupled formant shift — at vel 127 the formant
    /// frequencies are multiplied by `1 + velocity_formant_shift`.
    /// Default 0.15 = +15% (matches KubizBeat's KubyzVoice).
    pub velocity_formant_shift: f32,
    /// Where to park the Vox Mix knob when this preset is applied. Init =
    /// 0 (clean sine), Bashkir / Khomus = 1 (formant is the voice).
    pub default_vox_mix: f32,
}

/// Convert a dB amplitude (relative to harmonic 1) into a linear ratio.
const fn db_to_lin(db: f32) -> f32 {
    // 10^(db/20) — const-evaluable on stable since 1.83.
    // Power form spelled out with libm-style approximation… actually
    // const-eval doesn't support powf yet; we compute at runtime in the
    // builder below. Marker kept for clarity.
    db
}

/// Sample-target khomus — from `kubyz_sample_target.json`. Sample preset
/// in KubizBeat keeps formants disabled by default, so Mix can stay at 0.
fn harmonics_khomus_sample() -> [f32; N_HARMONICS] {
    let db: [f32; N_HARMONICS] = [
        0.0,  2.6, 17.6, 24.8, 25.4, 31.4, 19.6, 20.0,
        7.9, 19.7, 16.4, 16.8, 16.4,  8.0, 16.3,  3.1,
    ];
    let _ = db_to_lin(db[0]);
    db_to_lin_array(db)
}

/// Bashkir reference — from `bashkir_reference.json`. Formant 705/1301/2165.
fn harmonics_bashkir() -> [f32; N_HARMONICS] {
    let db: [f32; N_HARMONICS] = [
        0.0,  6.6, 19.0, 24.1, 17.0, 38.6,  9.4, 16.7,
       15.2, 17.9, 19.9,  9.8, 14.3, -0.5,  7.6,  3.3,
    ];
    db_to_lin_array(db)
}

/// "Init" preset — only harmonic 1, formants off. A clean sine you can
/// build on top of by tweaking individual harmonic bars in the GUI.
fn harmonics_init() -> [f32; N_HARMONICS] {
    let mut h = [0.0_f32; N_HARMONICS];
    h[0] = 1.0;
    h
}

/// Extracted from `/Users/rustam/Music/1music/my songs/Media/kubiz1000.wav`
/// via FFT + per-harmonic bin search (see analyse_kubyz.py).  Recording
/// is a real D2 jaw-harp tone — H1 fundamental is *quiet* compared to
/// the 11th–15th partials, which carry the whistling colour.
fn harmonics_real_d2() -> [f32; N_HARMONICS] {
    let db: [f32; N_HARMONICS] = [
         0.0, -33.83,  -9.50, -14.71, -16.52, -26.03, -11.07, -20.27,
       -16.22,  -9.19,  27.25,  -0.99,  34.42,  20.46,  45.10,  13.55,
    ];
    db_to_lin_array(db)
}

fn db_to_lin_array(db: [f32; N_HARMONICS]) -> [f32; N_HARMONICS] {
    // The JSON dB values are relative to H1 but can go EITHER direction:
    // for a jaw harp many overtones sit ABOVE the fundamental (mouth-cavity
    // resonances on harmonics 11–15 are the whistling part of the sound).
    // Convert as plain `10^(db/20)`, then normalise by the loudest harmonic
    // (not H1!) so the whole vector lives in [0, 1] regardless of which
    // partial dominates. That keeps the additive sum bounded without
    // throwing away the "loud overtone" character.
    let mut out = [0.0_f32; N_HARMONICS];
    for i in 0..N_HARMONICS {
        out[i] = 10f32.powf(db[i] / 20.0);
    }
    let max = out.iter().copied().fold(0.0_f32, f32::max).max(1e-6);
    for s in out.iter_mut() {
        *s /= max;
    }
    out
}

/// Bashkir formant — same numbers as `superduper_synth_core::formant`'s
/// `Bashkir Kubyz` entry, repeated here so the preset is self-contained.
const BASHKIR_FORMANT: FormantPreset = FormantPreset {
    name: "Bashkir",
    f: [705.0, 1301.0, 2165.0],
    bw: [200.0, 300.0, 400.0],
    gain: [1.0, 0.9, 0.75],
};
const KHOMUS_FORMANT: FormantPreset = FormantPreset {
    name: "Khomus Sample",
    f: [702.0, 1365.0, 2115.0],
    bw: [200.0, 300.0, 400.0],
    gain: [1.0, 0.9, 0.7],
};
const FORMANT_OFF: FormantPreset = FormantPreset {
    name: "Off",
    f: [700.0, 1200.0, 2600.0],
    bw: [200.0, 300.0, 400.0],
    gain: [1.0, 1.0, 1.0],
};
/// Resonance peaks measured from the real D2 recording — tight bandwidths
/// (50 Hz) because the cavity resonates on a single harmonic each.
const REAL_D2_FORMANT: FormantPreset = FormantPreset {
    name: "Real D2",
    f: [809.0, 1091.0, 1242.0],
    bw: [50.0, 50.0, 50.0],
    gain: [1.0, 1.0, 1.0],
};

pub fn presets() -> [KubyzPreset; 4] {
    [
        KubyzPreset {
            name: "Init (sine)",
            harmonics: harmonics_init(),
            formant: FORMANT_OFF,
            attack_s: 0.003,
            decay_s: 0.4,
            sustain: 0.3,
            release_s: 0.15,
            velocity_formant_shift: 0.15,
            default_vox_mix: 0.0,
        },
        // Plucked envelope: a real jaw harp's tongue is plucked once,
        // rings out, decays. NOT a piano with sustain — sustain must be 0
        // so the note dies on its own about a second after the strike.
        KubyzPreset {
            name: "Bashkir Kubyz",
            harmonics: harmonics_bashkir(),
            formant: BASHKIR_FORMANT,
            attack_s: 0.003,
            decay_s: 1.2,
            sustain: 0.0,
            release_s: 0.3,
            velocity_formant_shift: 0.15,
            default_vox_mix: 1.0,
        },
        KubyzPreset {
            name: "Khomus Sample",
            harmonics: harmonics_khomus_sample(),
            formant: KHOMUS_FORMANT,
            attack_s: 0.003,
            decay_s: 1.0,
            sustain: 0.0,
            release_s: 0.3,
            velocity_formant_shift: 0.15,
            default_vox_mix: 1.0,
        },
        KubyzPreset {
            name: "Real D2 (sample-fit)",
            harmonics: harmonics_real_d2(),
            formant: REAL_D2_FORMANT,
            attack_s: 0.003,
            decay_s: 1.8,
            sustain: 0.0,
            release_s: 0.3,
            velocity_formant_shift: 0.10,
            default_vox_mix: 0.5,
        },
    ]
}
