//! Auto-Tune-style pitch wheel + keyboard for SuperDuper Tune.
//!
//! Driven purely by the two shared readouts (`detected_hz`, `correction_st`):
//! the wheel shows the **note the voice is being tuned to** in the centre and an
//! **arc for how far / which way** the correction is pulling; the keyboard shows
//! that target note (blue), the note actually being sung (amber marker), and
//! which notes are in the current scale.

use egui::{Align2, Color32, FontId, Pos2, Rect, Stroke, Vec2};
use superduper_synth_core::gui as core_gui;

use crate::scale;

const BLUE: Color32 = Color32::from_rgb(90, 175, 245);
const AMBER: Color32 = Color32::from_rgb(232, 168, 74);
const WHITE_IN: Color32 = Color32::from_rgb(228, 234, 228);
const WHITE_OUT: Color32 = Color32::from_rgb(120, 132, 124);
const BLACK_KEY: Color32 = Color32::from_rgb(28, 36, 31);

fn tuned_midi(hz: f32, corr_st: f32) -> f32 {
    scale::hz_to_midi(hz) + corr_st
}

/// Central tuner circle: target note letter + a 0-centred correction arc.
pub fn draw_wheel(painter: &egui::Painter, area: Rect, hz: f32, corr_st: f32) {
    let c = area.center();
    let r = area.width().min(area.height()) * 0.46;

    painter.circle_filled(c, r, Color32::from_rgb(13, 19, 16));
    painter.circle_stroke(c, r, Stroke::new(1.5, core_gui::GREEN_FAINT));
    painter.circle_filled(c, r * 0.60, Color32::from_rgb(9, 14, 12));

    let top = -std::f32::consts::FRAC_PI_2;
    let span = 135f32.to_radians(); // ±100 cents → ±135°
    let at = |ang: f32, rad: f32| Pos2::new(c.x + rad * ang.cos(), c.y + rad * ang.sin());
    let gauge_r = r * 0.90;

    // Faint full-range gauge track (−100..+100) behind the correction arc.
    let track: Vec<Pos2> = (0..=80)
        .map(|i| at(top - span + 2.0 * span * (i as f32 / 80.0), gauge_r))
        .collect();
    painter.add(egui::Shape::line(track, Stroke::new(2.5, core_gui::GREEN_FAINT)));

    // Ticks at 0 / ±50 / ±100 cents (0 longer).
    for &cents in &[0.0f32, -50.0, 50.0, -100.0, 100.0] {
        let ang = top + (cents / 100.0) * span;
        let (inner, w) = if cents == 0.0 { (0.80, 2.0) } else { (0.85, 1.0) };
        painter.line_segment(
            [at(ang, r * 1.0), at(ang, r * inner)],
            Stroke::new(w, core_gui::GREEN_DIM),
        );
    }

    let voiced = hz >= 55.0;
    if voiced {
        // Correction arc from top (0) to the current deviation.
        let dev = (corr_st * 100.0).clamp(-115.0, 115.0);
        let end = top + (dev / 100.0) * span;
        let steps = 40usize;
        let pts: Vec<Pos2> = (0..=steps)
            .map(|i| at(top + (end - top) * (i as f32 / steps as f32), gauge_r))
            .collect();
        let col = if dev.abs() < 8.0 {
            core_gui::GREEN_BRIGHT
        } else if dev.abs() < 40.0 {
            Color32::from_rgb(224, 204, 96)
        } else {
            AMBER
        };
        painter.add(egui::Shape::line(pts, Stroke::new(4.0, col)));
        painter.circle_filled(at(end, gauge_r), 4.5, col);

        // Sung → target note names.
        let sung = scale::hz_to_midi(hz).round() as i32;
        let m = tuned_midi(hz, corr_st).round() as i32;
        let n = m.rem_euclid(12) as usize;
        let oct = m.div_euclid(12) - 1;

        // "from" note, small, high inside the ring.
        painter.text(
            Pos2::new(c.x, c.y - r * 0.44),
            Align2::CENTER_CENTER,
            scale::KEY_NAMES[sung.rem_euclid(12) as usize],
            FontId::monospace(13.0),
            core_gui::GREEN_DIM,
        );
        // Big target note letter + octave subscript.
        painter.text(
            c - Vec2::new(0.0, 8.0),
            Align2::CENTER_CENTER,
            scale::KEY_NAMES[n],
            FontId::monospace(46.0),
            BLUE,
        );
        painter.text(
            c + Vec2::new(0.0, 24.0),
            Align2::CENTER_CENTER,
            format!("oct {oct}"),
            FontId::monospace(11.0),
            core_gui::GREEN,
        );
        // Cents readout, clean, low inside the ring (no ring collision).
        painter.text(
            Pos2::new(c.x, c.y + r * 0.46),
            Align2::CENTER_CENTER,
            format!("{:+.0} cents", corr_st * 100.0),
            FontId::monospace(12.0),
            col,
        );
    } else {
        painter.text(
            c,
            Align2::CENTER_CENTER,
            "\u{2013}",
            FontId::monospace(40.0),
            core_gui::GREEN_DIM,
        );
    }
}

/// Two-octave keyboard (from C3). Highlights the tuned-to note (blue), marks the
/// note being sung (amber dot), tints in-scale keys.
pub fn draw_keyboard(
    painter: &egui::Painter,
    area: Rect,
    hz: f32,
    corr_st: f32,
    key: u8,
    scale_mask: u16,
) {
    const LOW: i32 = 48; // C3
    const OCTS: i32 = 2;
    let n_white = (7 * OCTS) as f32;
    let white_w = area.width() / n_white;

    let voiced = hz >= 55.0;
    let target = voiced.then(|| tuned_midi(hz, corr_st).round() as i32);
    let sung = voiced.then(|| scale::hz_to_midi(hz).round() as i32);
    let in_scale = |midi: i32| {
        let deg = (midi - key as i32).rem_euclid(12) as u32;
        scale_mask & (1u16 << deg) != 0
    };

    let white_semis = [0i32, 2, 4, 5, 7, 9, 11];
    // Black key → index of the white key it sits just right of, within the octave.
    let black: [(i32, i32); 5] = [(1, 0), (3, 1), (6, 3), (8, 4), (10, 5)];

    // White keys.
    let mut wi = 0i32;
    for o in 0..OCTS {
        for &s in &white_semis {
            let midi = LOW + o * 12 + s;
            let x = area.left() + wi as f32 * white_w;
            let kr = Rect::from_min_size(
                Pos2::new(x + 0.5, area.top()),
                Vec2::new(white_w - 1.0, area.height()),
            );
            let fill = if target == Some(midi) {
                BLUE
            } else if in_scale(midi) {
                WHITE_IN
            } else {
                WHITE_OUT
            };
            painter.rect_filled(kr, 2.0, fill);
            painter.line_segment(
                [Pos2::new(x, area.top()), Pos2::new(x, area.bottom())],
                Stroke::new(1.0, Color32::from_rgb(20, 26, 22)),
            );
            if sung == Some(midi) {
                painter.circle_filled(
                    Pos2::new(kr.center().x, area.bottom() - 8.0),
                    3.5,
                    AMBER,
                );
            }
            wi += 1;
        }
    }

    // Black keys on top.
    let black_w = white_w * 0.62;
    let black_h = area.height() * 0.62;
    for o in 0..OCTS {
        for &(semi, after_white) in &black {
            let midi = LOW + o * 12 + semi;
            let white_index = o * 7 + after_white + 1;
            let cx = area.left() + white_index as f32 * white_w;
            let kr = Rect::from_min_size(
                Pos2::new(cx - black_w * 0.5, area.top()),
                Vec2::new(black_w, black_h),
            );
            let fill = if target == Some(midi) {
                BLUE
            } else if in_scale(midi) {
                Color32::from_rgb(52, 66, 58)
            } else {
                BLACK_KEY
            };
            painter.rect_filled(kr, 2.0, fill);
            if sung == Some(midi) {
                painter.circle_filled(
                    Pos2::new(kr.center().x, kr.bottom() - 7.0),
                    3.0,
                    AMBER,
                );
            }
        }
    }
}
