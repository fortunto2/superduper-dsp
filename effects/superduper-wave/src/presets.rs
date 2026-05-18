//! Wavetable presets — defined as pure math formulas.
//!
//! Each preset declares TWO waveform functions; the user's `WT Pos` knob
//! linearly morphs between them. For preset that's just one waveform,
//! both functions are the same.
//!
//! Adding a preset = add a new line. The formula is the documentation.

use crate::{P_DETUNE, P_DRIVE, P_SUB, P_UNISON, P_WT_POS, PARAMS};
use core::f32::consts::TAU;

/// Pure waveform formula. Phase normalised to [0, 1); output in [-1, 1].
/// Must be a free `fn` (not a closure) so the preset table stays `const`.
pub type WaveFormula = fn(f32) -> f32;

/// A preset is two waveforms (morphed by WT Pos) plus optional default
/// param overrides. `overrides` is a sparse list of `(param_index, value)`.
#[derive(Clone, Copy)]
pub struct Preset {
    pub name: &'static str,
    /// WT Pos = 0 → frame_a, WT Pos = 1 → frame_b. Linear morph.
    pub frame_a: WaveFormula,
    pub frame_b: WaveFormula,
    /// Sparse default-param overrides (index, value).
    pub overrides: &'static [(usize, f32)],
}

impl Preset {
    /// Build the full default-value vector by starting from each `ParamDef`'s
    /// default and applying overrides on top.
    pub fn default_values(&self) -> [f32; PARAMS.len()] {
        let mut out = [0.0_f32; PARAMS.len()];
        for (i, p) in PARAMS.iter().enumerate() {
            out[i] = p.default as f32;
        }
        for &(idx, v) in self.overrides {
            if idx < out.len() {
                out[idx] = v;
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Building blocks — the math formulas themselves.
//
// All take phase ∈ [0, 1). All output amplitudes are scaled into a comfortable
// [-1, 1] range (no normalisation step in the oscillator; the formulas own
// that responsibility).
// ---------------------------------------------------------------------------

fn sine(p: f32) -> f32 {
    (p * TAU).sin()
}

fn saw(p: f32) -> f32 {
    2.0 * p - 1.0
}

fn square(p: f32) -> f32 {
    if p < 0.5 { 1.0 } else { -1.0 }
}

fn triangle(p: f32) -> f32 {
    if p < 0.5 { 4.0 * p - 1.0 } else { 3.0 - 4.0 * p }
}

fn pulse_25(p: f32) -> f32 {
    if p < 0.25 { 1.0 } else { -1.0 }
}

/// Saw + small 3rd harmonic — grittier than a plain saw.
fn fat_saw(p: f32) -> f32 {
    let s = saw(p);
    let h3 = (p * TAU * 3.0).sin() * 0.25;
    (s + h3) * 0.75
}

/// Two sines slightly detuned and stacked — classic Reese bass core.
fn reese(p: f32) -> f32 {
    let a = (p * TAU).sin();
    let b = (p * TAU * 1.005).sin();
    (a + b) * 0.5
}

/// FM-flavoured growl: sine carrier whose phase is modulated by a 2× sine.
fn fm_growl(p: f32) -> f32 {
    let mod_idx = 0.6;
    let modulator = (p * TAU * 2.0).sin();
    (p * TAU + mod_idx * modulator).sin()
}

/// 808-ish: pure sine then tanh-pushed for a bit of harmonic warmth.
fn sub_808(p: f32) -> f32 {
    ((p * TAU).sin() * 1.4).tanh()
}

/// First few harmonics weighted to peak around the 5th-6th — vowel-like.
fn formant_a(p: f32) -> f32 {
    let h1 = (p * TAU).sin() * 0.4;
    let h2 = (p * TAU * 2.0).sin() * 0.2;
    let h5 = (p * TAU * 5.0).sin() * 0.45;
    let h6 = (p * TAU * 6.0).sin() * 0.35;
    h1 + h2 + h5 + h6
}

/// Sawtooth with the upper half clipped — produces strong 3rd/5th
/// harmonics, classic "growly" bass core.
fn hard_saw(p: f32) -> f32 {
    let s = saw(p);
    s.clamp(-1.0, 0.5) * 1.3
}

// ---------------------------------------------------------------------------
// Preset table — math expressed as code, version-controlled, readable.
// Add a new line, recompile, restart the host, new preset is selectable.
// ---------------------------------------------------------------------------

pub const PRESETS: &[Preset] = &[
    Preset { name: "Init (Sine)",  frame_a: sine,      frame_b: sine,     overrides: &[] },
    Preset { name: "Saw",          frame_a: saw,       frame_b: saw,      overrides: &[] },
    Preset { name: "Square",       frame_a: square,    frame_b: square,   overrides: &[] },
    Preset { name: "Triangle",     frame_a: triangle,  frame_b: triangle, overrides: &[] },
    Preset { name: "Pulse 25%",    frame_a: pulse_25,  frame_b: pulse_25, overrides: &[] },

    // ---- Morphing pairs — turn WT Pos to hear them sweep. ----
    Preset { name: "Sine → Saw",      frame_a: sine,    frame_b: saw,    overrides: &[(P_WT_POS, 0.0)] },
    Preset { name: "Sine → Square",   frame_a: sine,    frame_b: square, overrides: &[(P_WT_POS, 0.0)] },
    Preset { name: "Saw → Square",    frame_a: saw,     frame_b: square, overrides: &[(P_WT_POS, 0.5)] },
    Preset { name: "Triangle → Saw",  frame_a: triangle,frame_b: saw,    overrides: &[(P_WT_POS, 0.0)] },

    // ---- Bass-oriented patches ----
    Preset {
        name: "Reese Bass",
        frame_a: reese,
        frame_b: saw,
        overrides: &[(P_UNISON, 5.0), (P_DETUNE, 18.0), (P_WT_POS, 0.3)],
    },
    Preset {
        name: "FM Growl",
        frame_a: fm_growl,
        frame_b: hard_saw,
        overrides: &[(P_DRIVE, 0.35), (P_WT_POS, 0.4)],
    },
    Preset {
        name: "808 Sub",
        frame_a: sub_808,
        frame_b: triangle,
        overrides: &[(P_SUB, 0.5), (P_DETUNE, 0.0), (P_UNISON, 1.0)],
    },
    Preset {
        name: "Fat Saw Lead",
        frame_a: fat_saw,
        frame_b: saw,
        overrides: &[(P_UNISON, 5.0), (P_DETUNE, 24.0)],
    },
    Preset {
        name: "Formant",
        frame_a: formant_a,
        frame_b: sine,
        overrides: &[],
    },
];
