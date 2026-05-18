//! Kubyz voice — additive 16-harmonic oscillator + ADSR + 3-band formant.
//!
//! Why additive instead of a wavetable: kubyz/khomus tone has a sharp
//! resonant character that's defined as much by the *which harmonics*
//! present as by the formant cavities of the player's mouth. Pre-baking
//! one wavetable would freeze the harmonics; doing additive lets us morph
//! between presets (and let the user redraw harmonics live).

use superduper_synth_core::dsp_blocks::AdsrEnvelope;
use superduper_synth_core::formant::Formant;

use crate::presets::N_HARMONICS;

/// One playable kubyz voice. Holds 16 sine-osc phases, the formant stage
/// and an amplitude ADSR.
#[derive(Clone, Copy)]
pub struct KubyzVoice {
    phases: [f32; N_HARMONICS],
    formant: Formant,
    pub env: AdsrEnvelope,
    pub key: u8,
    pub note_id: i32,
    pub velocity: f32,
    pub age_stamp: u64,
    pub choke_remaining: u32,
    pub choke_total: u32,
    pub choke_level: f32,
}

pub const NOTE_FREE: u8 = 0xff;

impl Default for KubyzVoice {
    fn default() -> Self {
        Self {
            phases: [0.0; N_HARMONICS],
            formant: Formant::default(),
            env: AdsrEnvelope::default(),
            key: NOTE_FREE,
            note_id: -1,
            velocity: 0.0,
            age_stamp: 0,
            choke_remaining: 0,
            choke_total: 0,
            choke_level: 0.0,
        }
    }
}

/// Per-sample params (small, cache-friendly).
#[derive(Copy, Clone)]
pub struct KubyzParams<'a> {
    pub sr: f32,
    pub root_hz: f32,
    pub harmonics: &'a [f32; N_HARMONICS],
    pub formant_f: [f32; 3],
    pub formant_bw: [f32; 3],
    pub formant_gain: [f32; 3],
    pub formant_mix: f32,
    /// Velocity-coupled formant shift: at vel=1, multiplies each F by
    /// `1 + velocity_formant_shift`. Matches KubyzVoice.swift.
    pub velocity_formant_shift: f32,
}

impl KubyzVoice {
    /// Render one stereo pair. Mono additive sum → stereo formant (each
    /// channel has independent biquad state for a touch of width).
    #[inline]
    pub fn process(&mut self, p: KubyzParams<'_>) -> (f32, f32) {
        // 1. Additive sum.
        let mut x = 0.0_f32;
        for n in 0..N_HARMONICS {
            let amp = p.harmonics[n];
            if amp.abs() < 1e-5 {
                continue;
            }
            let inc = p.root_hz * (n + 1) as f32 / p.sr;
            self.phases[n] += inc;
            if self.phases[n] >= 1.0 {
                self.phases[n] -= 1.0;
            }
            x += amp * (self.phases[n] * core::f32::consts::TAU).sin();
        }
        // 2. Normalise — after the preset's amplitudes are already capped
        // at 1.0 the worst-case sum of 16 sines is 16 (full alignment),
        // realistically ≤ 4 RMS. Scale by ¼ so a typical patch sits near
        // unity, then tanh-soft for the rare in-phase peaks.
        let drive = (x * 0.25).tanh();

        // 3. Velocity-shifted formant frequencies.
        let shift = 1.0 + self.velocity * p.velocity_formant_shift;
        let f = [
            p.formant_f[0] * shift,
            p.formant_f[1] * shift,
            p.formant_f[2] * shift,
        ];

        // 4. Apply formant (mix=0 = bypass, audible only when user opens it).
        self.formant.process(drive, drive, p.sr, f, p.formant_bw, p.formant_gain, p.formant_mix)
    }
}
