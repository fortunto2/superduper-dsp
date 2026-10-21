//! The spectrum panel: live output spectrum, one tinted region per band,
//! each band's gain reduction hanging from the top as a coloured bar, and the
//! crossovers as draggable vertical handles.

use std::sync::atomic::Ordering;

use superduper_synth_core::gui as core_gui;

use crate::dsp::{band_range, MAX_BANDS};
use crate::gui::{hz, set_param, GuiState, BAND_COLORS};
use crate::{bp, settings_from, F_BYPASS, F_KEY, P_XOVER};

const HEIGHT: f32 = 180.0;
/// GR that fills the panel's full height.
const GR_SCALE_DB: f32 = 24.0;
const GRAB_PX: f32 = 8.0;
const F_MIN: f32 = 20.0;
const F_MAX: f32 = 20_000.0;

// Same log mapping as core_gui::draw_spectrum_strip (at 48 kHz: 20 Hz–20 kHz).
fn f2x(f: f32, r: egui::Rect) -> f32 {
    r.left() + (f.clamp(F_MIN, F_MAX).ln() - F_MIN.ln()) / (F_MAX.ln() - F_MIN.ln()) * r.width()
}
fn x2f(x: f32, r: egui::Rect) -> f32 {
    let t = ((x - r.left()) / r.width()).clamp(0.0, 1.0);
    (F_MIN.ln() + t * (F_MAX.ln() - F_MIN.ln())).exp()
}

fn tint(c: egui::Color32, a: u8) -> egui::Color32 { egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a) }

pub(crate) fn draw(ui: &mut egui::Ui, state: &mut GuiState) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), HEIGHT), egui::Sense::click_and_drag());
    core_gui::draw_spectrum_strip(ui, &state.shared.scope, rect, 48_000.0);
    let sh = state.shared.clone();
    let s = settings_from(&sh.params);
    let painter = ui.painter_at(rect);

    for b in 0..s.bands {
        let (lo, hi) = band_range(s.bands, &s.xover_hz, b);
        let (x0, x1) = (f2x(lo, rect), f2x(hi, rect));
        let col = BAND_COLORS[b];
        let selected = state.selected_band == b;
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())),
            0.0,
            tint(col, if selected { 28 } else { 12 }),
        );
        // Gain change: compression hangs from the top, upward gain rises
        // from the bottom.
        let gr = sh.meters.gr_db[b].load(Ordering::Relaxed);
        let h = (gr.abs() / GR_SCALE_DB).clamp(0.0, 1.0) * rect.height();
        if h > 0.5 {
            let bar = if gr < 0.0 {
                egui::Rect::from_min_max(egui::pos2(x0 + 1.0, rect.top()), egui::pos2(x1 - 1.0, rect.top() + h))
            } else {
                egui::Rect::from_min_max(egui::pos2(x0 + 1.0, rect.bottom() - h), egui::pos2(x1 - 1.0, rect.bottom()))
            };
            painter.rect_filled(bar, 0.0, tint(col, 120));
        }
        label_band(&painter, &sh, rect, b, x0, x1, gr);
    }

    // Crossover handles.
    let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
    let near = |p: egui::Pos2| (0..s.bands - 1).map(|c| (c, (f2x(s.xover_hz[c], rect) - p.x).abs())).filter(|(_, d)| *d <= GRAB_PX).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(c, _)| c);
    let hovered = pointer.and_then(near);
    for c in 0..s.bands - 1 {
        let x = f2x(s.xover_hz[c], rect);
        let hot = state.drag == Some(c) || hovered == Some(c);
        let stroke = egui::Stroke::new(if hot { 2.0 } else { 1.2 }, if hot { core_gui::GREEN_BRIGHT } else { core_gui::GREEN });
        painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], stroke);
        painter.circle_filled(egui::pos2(x, rect.bottom() - 10.0), if hot { 6.0 } else { 4.5 }, stroke.color);
        painter.text(egui::pos2(x + 4.0, rect.bottom() - 22.0), egui::Align2::LEFT_BOTTOM, hz(s.xover_hz[c]), egui::FontId::monospace(10.0), core_gui::GREEN_DIM);
    }
    if hovered.is_some() || state.drag.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    interact(&resp, state, rect, &s.xover_hz, s.bands, near);
}

fn label_band(painter: &egui::Painter, sh: &crate::SharedParamsInner, rect: egui::Rect, b: usize, x0: f32, x1: f32, gr: f32) {
    let bypassed = sh.params[bp(b, F_BYPASS)].load(Ordering::Relaxed) >= 0.5;
    let key = sh.params[bp(b, F_KEY)].load(Ordering::Relaxed);
    let mut text = format!("B{} {:+.1}", b + 1, gr);
    if bypassed { text = format!("B{} byp", b + 1); }
    if key > 0.5 && !bypassed {
        let k = sh.meters.key_db[b].load(Ordering::Relaxed);
        text.push_str(&format!("\nkey {key:.0}% {}", if k > -119.0 { format!("{k:.0}dB") } else { "—".into() }));
    }
    let x = (x0 + 4.0).min(x1 - 4.0);
    painter.text(egui::pos2(x, rect.top() + 3.0), egui::Align2::LEFT_TOP, text, egui::FontId::monospace(10.0), BAND_COLORS[b]);
}

fn interact(
    resp: &egui::Response,
    state: &mut GuiState,
    rect: egui::Rect,
    xover: &[f32; 3],
    bands: usize,
    near: impl Fn(egui::Pos2) -> Option<usize>,
) {
    let sh = state.shared.clone();
    if resp.drag_started() {
        state.drag = resp.interact_pointer_pos().and_then(&near);
        if let Some(c) = state.drag { sh.gesture_begin[P_XOVER[c]].store(true, Ordering::Relaxed); }
    }
    if let (Some(c), Some(p)) = (state.drag, resp.interact_pointer_pos()) {
        if resp.dragged() {
            // Keep ½ octave from the neighbours (what the engine enforces) so
            // bands never cross and the shelves can deliver their gains.
            let r = crate::dsp::MIN_XOVER_RATIO;
            let lo = if c == 0 { F_MIN } else { xover[c - 1] * r };
            let hi = if c + 2 >= bands { F_MAX } else { xover[c + 1] / r };
            set_param(&sh, P_XOVER[c], x2f(p.x, rect).clamp(lo, hi).round());
        }
    }
    if resp.drag_stopped() {
        if let Some(c) = state.drag.take() { sh.gesture_end[P_XOVER[c]].store(true, Ordering::Relaxed); }
    }
    if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let f = x2f(p.x, rect);
            state.selected_band = (0..bands).find(|&b| f < band_range(bands, xover, b).1).unwrap_or(bands - 1).min(MAX_BANDS - 1);
        }
    }
}
