//! Direct-manipulation formant editor — every formant is a hill on a log
//! frequency axis, edited by grabbing it, the way a shape is in a vector
//! editor:
//!
//! - drag the hill (or its top handle): ← → = centre frequency, ↑ ↓ = gain.
//!   Shift while dragging locks the gain (horizontal move only);
//! - drag a side handle: bandwidth, symmetric around the centre (the two
//!   −3 dB points move together, like resizing a shape from its edge);
//! - double-click empty space: add a formant there (max 6);
//! - right-click a hill, or select it and press Delete / Backspace: remove
//!   it (min 1);
//! - hover highlights; hovered and selected hills show bw / dB / Q, and every
//!   hill always carries its centre in Hz so it can be read against the
//!   mix's spectrum.
//!
//! Behind the hills: the 16 harmonics at the last played note's pitch — a
//! dim stem for the dry level, a bright dot for roughly what comes out of
//! the formant at the current Vox Mix — and the summed bank response.
//!
//! How it relates to the vowel pad: bands 1–3 ARE the F1/F2/F3 params (see
//! `formants.rs`). The pad writes F1/F2 (and snaps F3), so the first three
//! hills move live under it; dragging hill 1 or 2 here writes the same params,
//! so the pad dot follows. One source of truth, two views. Bands 4–6 exist
//! only here. With fewer than two bands live the pad still writes F2, which
//! is simply not heard until a second band is added.
//!
//! Everything here runs on the GUI thread and touches the plugin only
//! through atomics (`formants::load` / `formants::store`).

use std::sync::atomic::Ordering;
use superduper_synth_core::dsp_blocks::midi_note_to_hz;
use superduper_synth_core::gui as core_gui;
use superduper_synth_core::kubyz::formant_bank::{
    bandpass_response, bank_magnitude, effective_q, Q_MAX, Q_MIN,
};

use crate::formants::{self, Band, FormantSet, GAIN_MAX, GAIN_MIN, PARAM_BANDS};
use crate::presets::N_HARMONICS;
use crate::trajectory::MouthShape;
use crate::{SharedParams, P_BRIGHT, P_F1, P_MOUTH_DEPTH, P_MOUTH_SHAPE, P_TONGUE_ST, P_VOX_MIX};

/// Frequency axis (log). Wide enough for F1's 80 Hz floor and F3's 6 kHz
/// ceiling with a little air on both sides.
pub const AXIS_F_MIN: f32 = 60.0;
pub const AXIS_F_MAX: f32 = 7000.0;
/// Level axis (dB, 0 = unity gain / harmonic amplitude 1.0).
pub const DB_TOP: f32 = 12.0;
pub const DB_BOTTOM: f32 = -42.0;
/// Handle radius as drawn, and the (larger) radius that grabs it.
const HANDLE_R: f32 = 5.0;
const HIT_R: f32 = 9.0;
/// Level of the side handles under the peak: the −3 dB points.
const EDGE_DB: f32 = -3.0103;
/// Mouth-trajectory excursion in Hz at depth 1 — must match lib.rs.
const MOUTH_EX_F1: f32 = 220.0;
const MOUTH_EX_F2: f32 = 600.0;

/// What part of a formant the pointer is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grab {
    /// The top handle: frequency + gain.
    Centre,
    /// The lower-frequency −3 dB handle: bandwidth.
    Left,
    /// The upper-frequency −3 dB handle: bandwidth.
    Right,
    /// The hill itself: behaves like the centre handle.
    Body,
}

/// An in-progress drag. The offsets keep the grabbed point under the
/// pointer instead of snapping the centre to it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drag {
    pub slot: usize,
    pub grab: Grab,
    /// log2(band f / pointer f) at the press.
    pub d_oct: f32,
    /// band dB − pointer dB at the press.
    pub d_db: f32,
}

/// Per-window editor state, kept in `GuiState`.
#[derive(Default)]
pub struct EditorState {
    selected: Option<usize>,
    drag: Option<Drag>,
}

// ---------------------------------------------------------------------------
// Pure math — mapping, band geometry, hit-testing, drag. Unit-tested below.
// ---------------------------------------------------------------------------

/// Frequency → 0..1 along the log axis.
pub fn freq_to_frac(f: f32) -> f32 {
    (f.max(1.0) / AXIS_F_MIN).ln() / (AXIS_F_MAX / AXIS_F_MIN).ln()
}

/// 0..1 along the log axis → frequency.
pub fn frac_to_freq(t: f32) -> f32 {
    AXIS_F_MIN * (AXIS_F_MAX / AXIS_F_MIN).powf(t)
}

pub fn gain_to_db(g: f32) -> f32 {
    20.0 * g.max(1e-6).log10()
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// The plotting rectangle and its two axes.
#[derive(Clone, Copy, Debug)]
pub struct Geom {
    pub rect: egui::Rect,
}

impl Geom {
    pub fn x_of(&self, f: f32) -> f32 {
        self.rect.left() + freq_to_frac(f) * self.rect.width()
    }
    pub fn f_of(&self, x: f32) -> f32 {
        frac_to_freq((x - self.rect.left()) / self.rect.width().max(1.0))
    }
    pub fn y_of(&self, db: f32) -> f32 {
        let t = (DB_TOP - db) / (DB_TOP - DB_BOTTOM);
        self.rect.top() + t * self.rect.height()
    }
    pub fn db_of(&self, y: f32) -> f32 {
        DB_TOP - (y - self.rect.top()) / self.rect.height().max(1.0) * (DB_TOP - DB_BOTTOM)
    }
}

/// The −3 dB points of a band as the filter really tunes it (Q clamp
/// included): `hi − lo = f / Q`, `lo · hi = f²`.
pub fn edges(f: f32, bw: f32) -> (f32, f32) {
    let w = f / effective_q(f, bw);
    let hi = w * 0.5 + (w * w * 0.25 + f * f).sqrt();
    (hi - w, hi)
}

/// Bandwidth whose −3 dB edge sits at `g` — the inverse of `edges`, valid
/// from either side of the centre.
pub fn bw_from_edge(f: f32, g: f32) -> f32 {
    (g - f * f / g.max(1.0)).abs()
}

/// Bandwidth limits the filter will honour at centre `f` (its Q clamp).
pub fn bw_limits(f: f32) -> (f32, f32) {
    ((f / Q_MAX).max(20.0), f / Q_MIN)
}

/// One band's level in dB at `freq`.
pub fn band_db_at(b: &Band, freq: f32) -> f32 {
    let (re, im) = bandpass_response(freq, b.f, effective_q(b.f, b.bw));
    gain_to_db(b.gain) + 10.0 * (re * re + im * im).max(1e-12).log10()
}

/// Screen positions of a band's three handles: centre, left, right.
pub fn handles(b: &Band, g: &Geom) -> [egui::Pos2; 3] {
    let peak = gain_to_db(b.gain);
    let (lo, hi) = edges(b.f, b.bw);
    let edge_y = g.y_of(peak + EDGE_DB);
    [
        egui::pos2(g.x_of(b.f), g.y_of(peak)),
        egui::pos2(g.x_of(lo), edge_y),
        egui::pos2(g.x_of(hi), edge_y),
    ]
}

/// What is under `p`. Handles win over hills; the `prefer`red (selected)
/// band's handles win over everyone else's; among hills the one whose curve
/// is highest at the pointer's frequency wins. A hill only counts within
/// 12 dB of its peak — its skirts reach the whole axis.
pub fn hit_test(set: &FormantSet, g: &Geom, p: egui::Pos2, prefer: Option<usize>) -> Option<(usize, Grab)> {
    let live = set.count;
    let order = prefer.filter(|&s| s < live).into_iter().chain((0..live).rev());
    for slot in order {
        let [c, l, r] = handles(&set.bands[slot], g);
        for (pos, grab) in [(c, Grab::Centre), (l, Grab::Left), (r, Grab::Right)] {
            if pos.distance(p) <= HIT_R {
                return Some((slot, grab));
            }
        }
    }
    let f = g.f_of(p.x);
    let pointer_db = g.db_of(p.y);
    let mut best: Option<(usize, f32)> = None;
    for slot in 0..live {
        let b = &set.bands[slot];
        let at = band_db_at(b, f);
        if pointer_db <= at && at >= gain_to_db(b.gain) - 12.0 && best.is_none_or(|(_, d)| at > d) {
            best = Some((slot, at));
        }
    }
    best.map(|(slot, _)| (slot, Grab::Body))
}

/// Start a drag on `slot` with the pointer at (`f`, `db`).
pub fn begin_drag(set: &FormantSet, slot: usize, grab: Grab, f: f32, db: f32) -> Drag {
    let b = &set.bands[slot];
    Drag { slot, grab, d_oct: (b.f / f.max(1.0)).log2(), d_db: gain_to_db(b.gain) - db }
}

/// Apply the pointer at (`f`, `db`) to the dragged band. `lock_gain` = Shift.
pub fn drag_to(set: &mut FormantSet, d: &Drag, f: f32, db: f32, lock_gain: bool) {
    let (lo, hi) = formants::slot_range(d.slot);
    let b = &mut set.bands[d.slot];
    match d.grab {
        Grab::Centre | Grab::Body => {
            b.f = (f * d.d_oct.exp2()).clamp(lo, hi);
            if !lock_gain {
                b.gain = db_to_gain(db + d.d_db).clamp(GAIN_MIN, GAIN_MAX);
            }
        }
        Grab::Left | Grab::Right => {
            // An edge dragged across the centre would flip sides: hold it.
            let g = if d.grab == Grab::Right { f.max(b.f * 1.001) } else { f.min(b.f * 0.999) };
            let (bw_lo, bw_hi) = bw_limits(b.f);
            b.bw = bw_from_edge(b.f, g).clamp(bw_lo, bw_hi);
        }
    }
}

/// A new band for a double-click at (`f`, `db`): Q ≈ 5, the clicked level.
pub fn new_band(f: f32, db: f32) -> Band {
    let (lo, hi) = bw_limits(f);
    Band { f, bw: (f * 0.2).clamp(lo, hi), gain: db_to_gain(db).clamp(GAIN_MIN, GAIN_MAX) }
}

// ---------------------------------------------------------------------------
// egui
// ---------------------------------------------------------------------------

/// Draw the editor in `rect` and handle its mouse / keyboard input.
pub fn draw(ui: &mut egui::Ui, shared: &SharedParams, rect: egui::Rect, st: &mut EditorState) {
    let g = Geom { rect: rect.shrink2(egui::vec2(6.0, 10.0)) };
    let response = ui.interact(rect, ui.id().with("kubyz_formant_editor"), egui::Sense::click_and_drag());
    let mut set = formants::load(shared);
    if st.selected.is_some_and(|s| s >= set.count) {
        st.selected = None;
    }
    let hover = response.hover_pos().and_then(|p| hit_test(&set, &g, p, st.selected));
    handle_input(ui, shared, &response, &g, &mut set, st, hover);

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, core_gui::PANEL_BG);
    painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, core_gui::GREEN_FAINT), egui::epaint::StrokeKind::Outside);
    paint_grid(&painter, &g);
    paint_harmonics(&painter, shared, &g, &set);
    let hot = st.drag.map(|d| d.slot).or(hover.map(|h| h.0));
    for slot in 0..set.count {
        let emphasis = if Some(slot) == st.selected { 2 } else { u8::from(Some(slot) == hot) };
        paint_band(&painter, &g, &set.bands[slot], slot, emphasis);
    }
    paint_sum(&painter, &g, &set);
    paint_mouth(&painter, shared, &g, &set);

    // Cursor feedback — the "this is grabbable / stretchable" cue.
    let icon = match (st.drag, hover) {
        (Some(d), _) if matches!(d.grab, Grab::Left | Grab::Right) => Some(egui::CursorIcon::ResizeHorizontal),
        (Some(_), _) => Some(egui::CursorIcon::Grabbing),
        (None, Some((_, Grab::Left | Grab::Right))) => Some(egui::CursorIcon::ResizeHorizontal),
        (None, Some(_)) => Some(egui::CursorIcon::Grab),
        _ => None,
    };
    if let Some(icon) = icon {
        ui.ctx().set_cursor_icon(icon);
    }
}

/// Mouse + keyboard. Writes go straight to the atomics via `formants::store`.
#[allow(clippy::too_many_arguments)]
fn handle_input(
    ui: &egui::Ui,
    shared: &SharedParams,
    response: &egui::Response,
    g: &Geom,
    set: &mut FormantSet,
    st: &mut EditorState,
    hover: Option<(usize, Grab)>,
) {
    let gesture = |slot: usize, flags: &[std::sync::atomic::AtomicBool]| {
        // Only F1–F3 are host params; a centre drag on them is a gesture.
        if slot < PARAM_BANDS {
            flags[P_F1 + slot].store(true, Ordering::Relaxed);
        }
    };
    if response.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or_default();
        if let Some((slot, grab)) = hit_test(set, g, origin, st.selected) {
            let d = begin_drag(set, slot, grab, g.f_of(origin.x), g.db_of(origin.y));
            if matches!(grab, Grab::Centre | Grab::Body) {
                gesture(slot, &shared.gesture_begin);
            }
            st.drag = Some(d);
            st.selected = Some(slot);
        }
    }
    if let (Some(d), Some(p)) = (st.drag, response.interact_pointer_pos()) {
        if response.dragged() {
            let lock = ui.input(|i| i.modifiers.shift);
            drag_to(set, &d, g.f_of(p.x), g.db_of(p.y), lock);
            formants::store(shared, set);
        }
    }
    if response.drag_stopped() {
        if let Some(d) = st.drag.take() {
            if matches!(d.grab, Grab::Centre | Grab::Body) {
                gesture(d.slot, &shared.gesture_end);
            }
        }
    }
    if response.clicked() {
        st.selected = hover.map(|h| h.0);
    }
    if response.double_clicked() && hover.is_none() {
        if let Some(p) = response.interact_pointer_pos() {
            if let Some(slot) = set.insert_sorted(new_band(g.f_of(p.x), g.db_of(p.y))) {
                formants::store(shared, set);
                st.selected = Some(slot);
            }
        }
    }
    let delete_key = ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
        && ui.ctx().memory(|m| m.focused().is_none());
    let victim = if response.secondary_clicked() { hover.map(|h| h.0) } else { None }
        .or(if delete_key { st.selected } else { None });
    if let Some(slot) = victim {
        if set.remove(slot) {
            formants::store(shared, set);
            st.selected = None;
            st.drag = None;
        }
    }
}

fn label(painter: &egui::Painter, pos: egui::Pos2, align: egui::Align2, text: &str, colour: egui::Color32) {
    painter.text(pos, align, text, egui::FontId::monospace(9.0), colour);
}

/// Frequency grid (labelled, so a formant can be read against a mix
/// analyser) and dB grid.
fn paint_grid(painter: &egui::Painter, g: &Geom) {
    let faint = egui::Stroke::new(0.5, core_gui::GREEN_FAINT);
    let r = g.rect;
    for (f, text) in [(100.0, "100"), (200.0, "200"), (500.0, "500"), (1000.0, "1k"), (2000.0, "2k"), (5000.0, "5k")] {
        let x = g.x_of(f);
        painter.line_segment([egui::pos2(x, r.top()), egui::pos2(x, r.bottom())], faint);
        label(painter, egui::pos2(x + 2.0, r.bottom() + 1.0), egui::Align2::LEFT_BOTTOM, text, core_gui::GREEN_DIM);
    }
    for db in [0.0_f32, -12.0, -24.0, -36.0] {
        let y = g.y_of(db);
        painter.line_segment([egui::pos2(r.left(), y), egui::pos2(r.right(), y)], faint);
        label(painter, egui::pos2(r.right(), y - 1.0), egui::Align2::RIGHT_BOTTOM, &format!("{db:+.0} dB"), core_gui::GREEN_FAINT);
    }
}

/// The 16 harmonics at the last played note: dim stem = dry amplitude
/// (Bright applied), dot = after the formant at the current Vox Mix
/// (magnitudes only, phase and the voice's soft-clip ignored).
fn paint_harmonics(painter: &egui::Painter, shared: &SharedParams, g: &Geom, set: &FormantSet) {
    let key = shared.last_key.load(Ordering::Relaxed) as f32;
    let root = midi_note_to_hz(key + shared.params[P_TONGUE_ST].load(Ordering::Relaxed));
    let bright = shared.params[P_BRIGHT].load(Ordering::Relaxed).max(0.0);
    let mix = shared.params[P_VOX_MIX].load(Ordering::Relaxed).clamp(0.0, 1.0);
    let bands = set.to_bands();
    for n in 0..N_HARMONICS {
        let f = root * (n + 1) as f32;
        if !(AXIS_F_MIN..=AXIS_F_MAX).contains(&f) {
            continue;
        }
        let amp = shared.harmonics[n].load(Ordering::Relaxed) * bright.powf(n as f32 / 4.0);
        if amp < 1e-4 {
            continue;
        }
        let x = g.x_of(f);
        let dry_y = g.y_of(gain_to_db(amp).max(DB_BOTTOM));
        painter.line_segment([egui::pos2(x, g.rect.bottom()), egui::pos2(x, dry_y)], egui::Stroke::new(2.0, core_gui::TRACK_BG));
        let out = amp * ((1.0 - mix) + mix * bank_magnitude(&bands, f));
        painter.circle_filled(egui::pos2(x, g.y_of(gain_to_db(out).max(DB_BOTTOM))), 2.5, core_gui::GREEN_BRIGHT);
    }
}

/// One hill: filled body, outline, width bar between the −3 dB handles,
/// three handles, and its label. `emphasis`: 0 idle, 1 hover, 2 selected.
fn paint_band(painter: &egui::Painter, g: &Geom, b: &Band, slot: usize, emphasis: u8) {
    let (fill_a, line) = match emphasis {
        2 => (70, core_gui::GREEN_BRIGHT),
        1 => (55, core_gui::GREEN_BRIGHT),
        _ => (35, core_gui::GREEN),
    };
    let fill = egui::Color32::from_rgba_unmultiplied(90, 180, 110, fill_a);
    let bottom = g.rect.bottom();
    let mut mesh = egui::Mesh::default();
    let mut outline = Vec::with_capacity(97);
    let (lo, hi) = (freq_to_frac(b.f / 8.0).max(0.0), freq_to_frac(b.f * 8.0).min(1.0));
    for i in 0..=96 {
        let f = frac_to_freq(lo + (hi - lo) * i as f32 / 96.0);
        let p = egui::pos2(g.x_of(f), g.y_of(band_db_at(b, f).max(DB_BOTTOM)));
        let base = mesh.vertices.len() as u32;
        mesh.colored_vertex(p, fill);
        mesh.colored_vertex(egui::pos2(p.x, bottom), fill);
        if i > 0 {
            mesh.add_triangle(base - 2, base - 1, base);
            mesh.add_triangle(base - 1, base, base + 1);
        }
        outline.push(p);
    }
    painter.add(mesh);
    painter.add(egui::Shape::line(outline, egui::Stroke::new(if emphasis > 0 { 1.6 } else { 1.0 }, line)));

    let [c, l, r] = handles(b, g);
    painter.line_segment([l, r], egui::Stroke::new(1.0, line));
    for p in [l, r] {
        painter.rect_filled(egui::Rect::from_center_size(p, egui::vec2(7.0, 7.0)), 1.0, line);
    }
    painter.circle_filled(c, if emphasis > 0 { HANDLE_R + 1.5 } else { HANDLE_R }, line);
    painter.circle_stroke(c, HANDLE_R + 2.5, egui::Stroke::new(1.0, core_gui::PANEL_BG));

    // Centre always labelled; details while hovered / selected.
    let head = format!("F{} {:.0} Hz", slot + 1, b.f);
    label(painter, egui::pos2(c.x, c.y - 9.0), egui::Align2::CENTER_BOTTOM, &head, line);
    if emphasis > 0 {
        let q = effective_q(b.f, b.bw);
        let detail = format!("bw {:.0} Hz · {:+.1} dB · Q {q:.1}", b.f / q, gain_to_db(b.gain));
        label(painter, egui::pos2(c.x, c.y - 20.0), egui::Align2::CENTER_BOTTOM, &detail, core_gui::GREEN_BRIGHT);
    }
}

/// The summed bank response (with make-up gain) — what the hills add up to.
fn paint_sum(painter: &egui::Painter, g: &Geom, set: &FormantSet) {
    let bands = set.to_bands();
    let pts: Vec<egui::Pos2> = (0..=160)
        .map(|i| {
            let f = frac_to_freq(i as f32 / 160.0);
            egui::pos2(g.x_of(f), g.y_of(gain_to_db(bank_magnitude(&bands, f)).max(DB_BOTTOM)))
        })
        .collect();
    painter.add(egui::Shape::dashed_line(&pts, egui::Stroke::new(1.0, core_gui::GREEN_BRIGHT), 4.0, 3.0));
}

/// Mouth trajectory reach on F1 / F2 (only with Mouth Depth > 0): a span
/// under each peak plus a dot where the trajectory has them right now.
fn paint_mouth(painter: &egui::Painter, shared: &SharedParams, g: &Geom, set: &FormantSet) {
    let depth = shared.params[P_MOUTH_DEPTH].load(Ordering::Relaxed).clamp(0.0, 1.0);
    if depth <= 0.001 {
        return;
    }
    let shape = MouthShape::from_index(shared.params[P_MOUTH_SHAPE].load(Ordering::Relaxed) as u32);
    let (tx, ty) = shape.point(shared.mouth_phase.load(Ordering::Relaxed));
    for (slot, ex, now) in [(0, MOUTH_EX_F1, ty), (1, MOUTH_EX_F2, tx)] {
        if slot >= set.count {
            continue;
        }
        let b = &set.bands[slot];
        let y = g.y_of(gain_to_db(b.gain)) + 12.0;
        let span = ex * depth;
        let (x0, x1) = (g.x_of((b.f - span).max(AXIS_F_MIN)), g.x_of(b.f + span));
        painter.line_segment([egui::pos2(x0, y), egui::pos2(x1, y)], egui::Stroke::new(1.0, core_gui::GREEN_DIM));
        painter.circle_filled(egui::pos2(g.x_of((b.f + now * span).max(AXIS_F_MIN)), y), 3.0, core_gui::GREEN_BRIGHT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formants::FREE_DEFAULTS;

    fn geom() -> Geom {
        Geom { rect: egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(600.0, 200.0)) }
    }

    fn set3() -> FormantSet {
        let mut bands = [Band { f: 1000.0, bw: 200.0, gain: 1.0 }; formants::MAX_FORMANTS];
        bands[0] = Band { f: 705.0, bw: 200.0, gain: 1.0 };
        bands[1] = Band { f: 1301.0, bw: 300.0, gain: 0.9 };
        bands[2] = Band { f: 2165.0, bw: 400.0, gain: 0.75 };
        bands[3..].copy_from_slice(&FREE_DEFAULTS);
        FormantSet { bands, count: 3 }
    }

    #[test]
    fn freq_and_level_mappings_invert() {
        let g = geom();
        for f in [60.0, 80.0, 440.0, 1301.0, 6000.0, 7000.0] {
            assert!((g.f_of(g.x_of(f)) - f).abs() / f < 1e-4, "{f}");
        }
        assert!((g.x_of(AXIS_F_MIN) - g.rect.left()).abs() < 1e-3);
        assert!((g.x_of(AXIS_F_MAX) - g.rect.right()).abs() < 1e-3);
        // Log axis: every octave is the same width.
        assert!(((g.x_of(200.0) - g.x_of(100.0)) - (g.x_of(4000.0) - g.x_of(2000.0))).abs() < 1e-3);
        for db in [DB_TOP, 0.0, -17.5, DB_BOTTOM] {
            assert!((g.db_of(g.y_of(db)) - db).abs() < 1e-3);
        }
        assert!((gain_to_db(db_to_gain(-6.0)) + 6.0).abs() < 1e-4);
    }

    #[test]
    fn edges_and_bandwidth_invert() {
        let (lo, hi) = edges(1000.0, 200.0);
        assert!((hi - lo - 200.0).abs() < 1e-2 && (lo * hi - 1e6).abs() < 1.0);
        assert!((bw_from_edge(1000.0, hi) - 200.0).abs() < 1e-2);
        assert!((bw_from_edge(1000.0, lo) - 200.0).abs() < 1e-2, "either edge gives the same width");
        // Q clamp shows up in the geometry: a 5 Hz band draws at f / 12.
        let (lo, hi) = edges(1200.0, 5.0);
        assert!((hi - lo - 100.0).abs() < 1e-2);
    }

    #[test]
    fn hit_test_finds_handles_body_and_nothing() {
        let g = geom();
        let s = set3();
        let [c, l, r] = handles(&s.bands[1], &g);
        assert_eq!(hit_test(&s, &g, c, None), Some((1, Grab::Centre)));
        assert_eq!(hit_test(&s, &g, l + egui::vec2(3.0, 0.0), None), Some((1, Grab::Left)));
        assert_eq!(hit_test(&s, &g, r, None), Some((1, Grab::Right)));
        // Just under F1's peak, between its edges: the body.
        let [c0, ..] = handles(&s.bands[0], &g);
        assert_eq!(hit_test(&s, &g, c0 + egui::vec2(0.0, 25.0), None), Some((0, Grab::Body)));
        // Far above every peak: empty space (where a double-click adds).
        assert_eq!(hit_test(&s, &g, egui::pos2(g.x_of(4500.0), g.y_of(DB_TOP - 1.0)), None), None);
        // Dead bands are not hit.
        let [c3, ..] = handles(&s.bands[3], &g);
        assert_ne!(hit_test(&s, &g, c3, None).map(|h| h.0), Some(3));
    }

    #[test]
    fn drag_moves_without_jump_and_respects_limits() {
        let mut s = set3();
        // Grab F2's body 20 % below its centre and 6 dB under its peak.
        let d = begin_drag(&s, 1, Grab::Body, 1301.0 * 0.8, gain_to_db(0.9) - 6.0);
        drag_to(&mut s, &d, 1301.0 * 0.8, gain_to_db(0.9) - 6.0, false);
        assert!((s.bands[1].f - 1301.0).abs() < 0.05 && (s.bands[1].gain - 0.9).abs() < 1e-4, "no jump on grab");
        drag_to(&mut s, &d, 2000.0 * 0.8, gain_to_db(0.9) - 12.0, true);
        assert!((s.bands[1].f - 2000.0).abs() < 0.1);
        assert!((s.bands[1].gain - 0.9).abs() < 1e-4, "Shift locks the gain");
        drag_to(&mut s, &d, 50_000.0, 40.0, false);
        assert_eq!(s.bands[1].f, formants::slot_range(1).1, "clamped to F2's range");
        assert_eq!(s.bands[1].gain, GAIN_MAX);
        // Right edge: stretch, then an edge dragged past the centre stays sane.
        let e = begin_drag(&s, 0, Grab::Right, 0.0, 0.0);
        drag_to(&mut s, &e, 900.0, 0.0, false);
        assert!((s.bands[0].bw - bw_from_edge(705.0, 900.0)).abs() < 1e-2);
        drag_to(&mut s, &e, 100.0, 0.0, false);
        assert_eq!(s.bands[0].bw, bw_limits(705.0).0);
    }

    #[test]
    fn new_band_is_sane() {
        let b = new_band(2500.0, -6.0);
        assert!((b.bw - 500.0).abs() < 1e-3 && (gain_to_db(b.gain) + 6.0).abs() < 1e-3);
        assert_eq!(new_band(1000.0, -80.0).gain, GAIN_MIN);
    }
}
