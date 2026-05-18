//! 2-D mouth trajectory shapes for Kubyz.
//!
//! Each shape is a closed (or near-closed) curve parameterised by `phase`
//! ∈ [0, 1) → (x, y) ∈ [-1, 1]². A LFO advances the phase and the
//! resulting offset is summed into the formant centre (F1, F2) so the
//! cursor walks around the vowel chart on its own — emulating an active
//! player's tongue/mouth motion.

use core::f32::consts::TAU;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouthShape {
    /// Steady circle around the centre.
    Circle,
    /// Horizontal sweep (X = saw, Y = small sine wobble).
    Sine,
    /// Figure-eight (lemniscate of Gerono).
    Figure8,
    /// Bouncing diagonal — X linear sweep, Y triangle.
    Triangle,
    /// One-dimensional left-right line.
    Line,
}

impl MouthShape {
    pub fn from_index(i: u32) -> Self {
        match i {
            1 => Self::Sine,
            2 => Self::Figure8,
            3 => Self::Triangle,
            4 => Self::Line,
            _ => Self::Circle,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Circle => "Circle",
            Self::Sine => "Sine",
            Self::Figure8 => "Figure-8",
            Self::Triangle => "Triangle",
            Self::Line => "Line",
        }
    }

    /// Sample the shape at `phase` ∈ [0, 1). Output is `(x, y)` ∈ [-1, 1]².
    /// Centre of every shape is (0, 0); caller scales by depth and adds
    /// the formant centre.
    #[inline]
    pub fn point(self, phase: f32) -> (f32, f32) {
        let p = phase.rem_euclid(1.0);
        match self {
            Self::Circle => {
                let a = p * TAU;
                (a.cos(), a.sin())
            }
            Self::Sine => {
                // Slow X sweep, fast Y wiggle — like jaw opening with a
                // tongue flick on top.
                let x = (2.0 * p - 1.0).clamp(-1.0, 1.0);
                let y = (p * TAU * 4.0).sin() * 0.6;
                (x, y)
            }
            Self::Figure8 => {
                // Lemniscate of Gerono — clean figure-8 visible on the pad.
                let a = p * TAU;
                let x = a.cos();
                let y = a.cos() * a.sin();
                (x, y)
            }
            Self::Triangle => {
                // X linear back-and-forth, Y triangular bounce at 2× rate.
                let x = if p < 0.5 { 4.0 * p - 1.0 } else { 3.0 - 4.0 * p };
                let pp = (p * 2.0) % 1.0;
                let y = if pp < 0.5 { 4.0 * pp - 1.0 } else { 3.0 - 4.0 * pp };
                (x, y * 0.7)
            }
            Self::Line => {
                // Symmetric back-and-forth along X, Y stays 0.
                let x = if p < 0.5 { 4.0 * p - 1.0 } else { 3.0 - 4.0 * p };
                (x, 0.0)
            }
        }
    }
}
