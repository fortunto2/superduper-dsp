//! Kubyz voice — additive 16-harmonic oscillator + ADSR + 3-band formant.
//!
//! Why additive instead of a wavetable: kubyz/khomus tone has a sharp
//! resonant character that's defined as much by the *which harmonics*
//! present as by the formant cavities of the player's mouth. Pre-baking
//! one wavetable would freeze the harmonics; doing additive lets us morph
//! between presets (and let the user redraw harmonics live).

use crate::dsp_blocks::{AdsrEnvelope, DcBlocker};
use crate::formant::Formant;

use super::N_HARMONICS;

/// One playable kubyz voice. Holds 16 sine-osc phases, the formant stage
/// and an amplitude ADSR.
#[derive(Clone, Copy)]
pub struct KubyzVoice {
    phases: [f32; N_HARMONICS],
    formant: Formant,
    /// First-order high-pass after the formant stack — catches any DC
    /// the tanh/BPF chain might leave behind, which otherwise reads as
    /// a quiet background rumble.
    dc_block_l: DcBlocker,
    dc_block_r: DcBlocker,
    pub env: AdsrEnvelope,
    pub key: u8,
    pub note_id: i32,
    pub velocity: f32,
    pub age_stamp: u64,
    pub choke_remaining: u32,
    pub choke_total: u32,
    pub choke_level: f32,
    /// Deferred-steal parking. A busy voice that gets stolen is choke-faded to
    /// silence (old note keeps sounding during the fade) and the new note is
    /// parked here; the render loop starts it from silence when the fade ends,
    /// so the new note never steps the waveform at full amplitude.
    /// `pending_key == NOTE_FREE` means nothing parked.
    pub pending_key: u8,
    pub pending_note_id: i32,
    pub pending_velocity: f32,
    /// A NoteOff can arrive for a note that is still parked behind the choke
    /// fade — every event in the block is drained before rendering, and the
    /// fade is only ~4 ms. Matching that NoteOff against `key` (which still
    /// holds the note being faded OUT) consumed the release and the parked
    /// note then sounded forever. Wind hit this first; Wave and Kubyz carried
    /// the same hole until it was found by review.
    pub pending_released: bool,
    /// Sample-counter for the on-note amplitude fade. Counts down from
    /// `note_fade_total` to 0; while > 0 the voice output is multiplied
    /// by `(total - remaining) / total` so the first ~2 ms of every
    /// note ramps in smoothly even when the ADSR's attack is 0 ms.
    note_fade_remaining: u32,
    note_fade_total: u32,
}

pub const NOTE_FREE: u8 = 0xff;

impl Default for KubyzVoice {
    fn default() -> Self {
        Self {
            phases: [0.0; N_HARMONICS],
            formant: Formant::default(),
            dc_block_l: DcBlocker::default(),
            dc_block_r: DcBlocker::default(),
            env: AdsrEnvelope::default(),
            key: NOTE_FREE,
            note_id: -1,
            velocity: 0.0,
            age_stamp: 0,
            choke_remaining: 0,
            choke_total: 0,
            choke_level: 0.0,
            pending_key: NOTE_FREE,
            pending_note_id: -1,
            pending_velocity: 0.0,
            pending_released: false,
            note_fade_remaining: 0,
            note_fade_total: 0,
        }
    }
}

impl KubyzVoice {
    /// Called from the host plumbing on every NoteOn.  Scatters the
    /// 16 oscillator phases (golden-ratio walk) so they don't all start
    /// in lockstep and produce a noticeable transient on the first
    /// sample.  Also kicks off a 2 ms amplitude ramp — separate from
    /// the ADSR's attack — which kills any residual click no matter
    /// how short the user set Attack to.
    pub fn on_note_on(&mut self, sample_rate: f32) {
        let phi = 0.618_033_988_5_f32;
        let mut p = 0.137_f32;
        for ph in self.phases.iter_mut() {
            *ph = p;
            p = (p + phi).fract();
        }
        let fade_samples = (sample_rate * 0.002) as u32;
        self.note_fade_total = fade_samples.max(1);
        self.note_fade_remaining = self.note_fade_total;
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
    /// channel has independent biquad state for a touch of width). The
    /// on-note 2 ms fade and a DC block on the way out kill the quiet
    /// transient + rumble the user reported as "lёгкий треск".
    #[inline]
    pub fn process_inner(&mut self, p: KubyzParams<'_>) -> (f32, f32) {
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

    /// Public render entry point — wraps `process_inner` with the on-note
    /// ramp and the DC blocker so callers don't have to know about either.
    #[inline]
    pub fn process(&mut self, p: KubyzParams<'_>) -> (f32, f32) {
        let (mut l, mut r) = self.process_inner(p);
        if self.note_fade_remaining > 0 {
            let fade = 1.0
                - (self.note_fade_remaining as f32) / (self.note_fade_total as f32);
            l *= fade;
            r *= fade;
            self.note_fade_remaining -= 1;
        }
        // Strip DC residue (formant BPF + tanh chain occasionally bias
        // the output a few millivolts, which adds a quiet rumble).
        (self.dc_block_l.process(l), self.dc_block_r.process(r))
    }
}
