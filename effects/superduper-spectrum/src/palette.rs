//! Color ramps for the spectrogram — `f32` magnitude in dB → RGB.
//!
//! Three styles, like iZotope Insight: warm "heat" (sonograph classic),
//! cool "phosphor" (vintage scope green), and neutral mono (engineering).

use egui::Color32;

#[derive(Copy, Clone, Debug)]
pub enum Palette {
    Phosphor = 0,
    Heat = 1,
    Mono = 2,
}

impl Palette {
    pub fn from_index(i: u32) -> Self {
        match i {
            1 => Self::Heat,
            2 => Self::Mono,
            _ => Self::Phosphor,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Phosphor => "Phosphor",
            Self::Heat => "Heat",
            Self::Mono => "Mono",
        }
    }
}

/// Map a dB value into a colour. `db` is clamped to `[min_db, 0]` first;
/// values below `min_db` collapse to fully transparent / black so quiet
/// background doesn't glow.
pub fn db_to_color(db: f32, min_db: f32, palette: Palette) -> Color32 {
    let t = ((db - min_db) / (0.0 - min_db)).clamp(0.0, 1.0);
    match palette {
        Palette::Phosphor => phosphor(t),
        Palette::Heat => heat(t),
        Palette::Mono => mono(t),
    }
}

/// Old-CRT phosphor ramp: black → forest → bright green → near-white.
fn phosphor(t: f32) -> Color32 {
    // Anchor stops. (t_at, r, g, b)
    let stops = [
        (0.00, 6.0, 10.0, 8.0),
        (0.25, 12.0, 60.0, 30.0),
        (0.50, 50.0, 140.0, 70.0),
        (0.75, 130.0, 220.0, 140.0),
        (1.00, 230.0, 255.0, 220.0),
    ];
    ramp(t, &stops)
}

/// "Heat" ramp like Insight's default sonograph: black → blue → magenta →
/// yellow → near-white. Used to be standard scientific colormap before
/// viridis came around; still the most readable for audio.
fn heat(t: f32) -> Color32 {
    let stops = [
        (0.00, 4.0, 6.0, 14.0),
        (0.20, 30.0, 50.0, 140.0),
        (0.40, 110.0, 50.0, 180.0),
        (0.60, 230.0, 80.0, 110.0),
        (0.80, 250.0, 200.0, 60.0),
        (1.00, 255.0, 250.0, 230.0),
    ];
    ramp(t, &stops)
}

/// Neutral grayscale.
fn mono(t: f32) -> Color32 {
    let v = (t * 255.0) as u8;
    Color32::from_rgb(v, v, v)
}

fn ramp(t: f32, stops: &[(f32, f32, f32, f32)]) -> Color32 {
    if t <= stops[0].0 {
        return rgb(stops[0]);
    }
    if t >= stops[stops.len() - 1].0 {
        return rgb(stops[stops.len() - 1]);
    }
    for w in stops.windows(2) {
        let a = w[0];
        let b = w[1];
        if t >= a.0 && t <= b.0 {
            let alpha = (t - a.0) / (b.0 - a.0);
            return Color32::from_rgb(
                lerp(a.1, b.1, alpha) as u8,
                lerp(a.2, b.2, alpha) as u8,
                lerp(a.3, b.3, alpha) as u8,
            );
        }
    }
    Color32::BLACK
}

fn rgb(s: (f32, f32, f32, f32)) -> Color32 {
    Color32::from_rgb(s.1 as u8, s.2 as u8, s.3 as u8)
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
