use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{
    apply_preset, push_custom_frame_a, PARAMS, P_ANTIALIAS, P_ATTACK, P_CUTOFF, P_DECAY, P_DETUNE,
    P_DRIVE, P_FENV_A, P_FENV_AMOUNT, P_FENV_D, P_FENV_R, P_FENV_S, P_FILTER_MODE, P_LFO_DEPTH,
    P_LFO_DEST, P_LFO_RATE, P_LFO_SHAPE, P_NOISE, P_OUTPUT, P_RELEASE, P_RESONANCE, P_SUB,
    P_SUSTAIN, P_UNISON, P_WT_POS, SharedParams,
};
use crate::osc::{mip_from_table, render_formula, WT_SIZE};

pub const DEFAULT_WIDTH: u32 = 760;
pub const DEFAULT_HEIGHT: u32 = 720;
pub const MIN_WIDTH: u32 = 540;
pub const MIN_HEIGHT: u32 = 520;
pub const MAX_WIDTH: u32 = 1400;
pub const MAX_HEIGHT: u32 = 1200;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

// ---------------------------------------------------------------------------
// CurveNodes — vector-editor-style nodes with sharp/smooth flag per node.
// Smooth segments use Catmull-Rom interpolation; sharp segments use linear.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct CurveNode {
    x: f32, // phase ∈ [0,1]
    y: f32, // amplitude ∈ [-1,1]
    /// `true` = participate in spline interpolation with smooth neighbours.
    /// `false` = produce a sharp corner.
    smooth: bool,
}

#[derive(Clone, Default)]
struct CurveNodes {
    pts: Vec<CurveNode>,
}

impl CurveNodes {
    fn from_table(table: &[f32], n: usize) -> Self {
        let mut pts = Vec::with_capacity(n);
        for i in 0..n {
            let x = i as f32 / (n - 1) as f32;
            let idx = (x * (table.len() - 1) as f32) as usize;
            pts.push(CurveNode {
                x,
                y: table[idx].clamp(-1.0, 1.0),
                smooth: false,
            });
        }
        Self { pts }
    }
    fn sort(&mut self) {
        self.pts
            .sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
    }

    /// Tangent at node `i`, **viewed from its left side** (i.e. for the
    /// segment ending at `i`).  Sharp nodes have zero tangent on the side
    /// of the corner; smooth nodes use the Fritsch-Carlson monotone
    /// approximation of the centred difference, which never overshoots.
    fn tangent_left(&self, i: usize) -> f32 {
        let n = self.pts.len();
        if !self.pts[i].smooth {
            return 0.0;
        }
        if i == 0 {
            // First-node has no left neighbour; mirror the right-side slope.
            let p0 = self.pts[0];
            let p1 = self.pts[1];
            return (p1.y - p0.y) / (p1.x - p0.x).max(1e-6);
        }
        if i + 1 >= n {
            let p0 = self.pts[i - 1];
            let p1 = self.pts[i];
            return (p1.y - p0.y) / (p1.x - p0.x).max(1e-6);
        }
        Self::fc_slope(self.pts[i - 1], self.pts[i], self.pts[i + 1])
    }
    fn tangent_right(&self, i: usize) -> f32 {
        let n = self.pts.len();
        if !self.pts[i].smooth {
            return 0.0;
        }
        if i + 1 >= n {
            let p0 = self.pts[i - 1];
            let p1 = self.pts[i];
            return (p1.y - p0.y) / (p1.x - p0.x).max(1e-6);
        }
        if i == 0 {
            let p0 = self.pts[0];
            let p1 = self.pts[1];
            return (p1.y - p0.y) / (p1.x - p0.x).max(1e-6);
        }
        Self::fc_slope(self.pts[i - 1], self.pts[i], self.pts[i + 1])
    }

    /// Fritsch-Carlson monotone slope at the middle node of (p0, p1, p2).
    /// Picks the harmonic-mean-flavoured tangent that keeps the resulting
    /// Hermite spline monotone on either side of the node — i.e. no
    /// overshoot past the user's drawn values, no spurious bulges.
    fn fc_slope(p0: CurveNode, p1: CurveNode, p2: CurveNode) -> f32 {
        let h_l = (p1.x - p0.x).max(1e-6);
        let h_r = (p2.x - p1.x).max(1e-6);
        let d_l = (p1.y - p0.y) / h_l;
        let d_r = (p2.y - p1.y) / h_r;
        if d_l * d_r <= 0.0 {
            // Local extremum or flat — flat tangent → preserves the shape.
            0.0
        } else {
            // Weighted harmonic mean (de Boor's formula).
            let w1 = 2.0 * h_r + h_l;
            let w2 = h_r + 2.0 * h_l;
            (w1 + w2) / (w1 / d_l + w2 / d_r)
        }
    }

    /// Cubic Hermite interpolation between two nodes given the tangents at
    /// each end (already scaled by the segment's x-span externally).
    #[inline]
    fn hermite(y0: f32, m0: f32, y1: f32, m1: f32, t: f32) -> f32 {
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 =  2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 =        t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 =        t3 -       t2;
        h00 * y0 + h10 * m0 + h01 * y1 + h11 * m1
    }

    /// Render a WT_SIZE-long buffer.
    /// Per segment the rule is:
    ///   * both endpoints sharp → pure linear (preserves draw exactly)
    ///   * otherwise → cubic Hermite with each node's own tangent (0 if
    ///     that node is sharp, FC-slope if smooth)
    fn render(&self) -> Arc<[f32; WT_SIZE]> {
        let mut buf = Box::new([0.0_f32; WT_SIZE]);
        if self.pts.len() < 2 {
            let v = self.pts.first().map(|n| n.y).unwrap_or(0.0);
            for slot in buf.iter_mut() {
                *slot = v;
            }
            return Arc::from(buf);
        }

        // Precompute per-segment endpoint tangents once.
        let n = self.pts.len();
        let mut m_right = vec![0.0_f32; n];
        let mut m_left = vec![0.0_f32; n];
        for i in 0..n {
            m_left[i] = self.tangent_left(i);
            m_right[i] = self.tangent_right(i);
        }

        for (i, slot) in buf.iter_mut().enumerate() {
            let x = i as f32 / WT_SIZE as f32;
            // Find the segment that contains x.
            let mut j = 0;
            for k in 0..self.pts.len() - 1 {
                if self.pts[k].x <= x && x <= self.pts[k + 1].x {
                    j = k;
                    break;
                }
                if k == self.pts.len() - 2 {
                    j = k;
                }
            }
            let p1 = self.pts[j];
            let p2 = self.pts[j + 1];
            let span = (p2.x - p1.x).max(1e-6);
            let t = ((x - p1.x) / span).clamp(0.0, 1.0);
            let y = if !p1.smooth && !p2.smooth {
                p1.y * (1.0 - t) + p2.y * t
            } else {
                // Tangents on this segment must be expressed as dy/dt (not dy/dx),
                // so multiply by the segment's x-span.
                let m0 = m_right[j] * span;
                let m1 = m_left[j + 1] * span;
                Self::hermite(p1.y, m0, p2.y, m1, t)
            };
            *slot = y.clamp(-1.5, 1.5);
        }
        let peak = buf.iter().map(|x| x.abs()).fold(0.0_f32, f32::max);
        if peak > 1.0 {
            let s = 1.0 / peak;
            for v in buf.iter_mut() {
                *v *= s;
            }
        }
        Arc::from(buf)
    }

    /// Ramer-Douglas-Peucker simplification.  `epsilon` is the maximum
    /// perpendicular distance a point may sit from the line between its
    /// preserved neighbours before it gets dropped — small epsilon keeps
    /// more detail, large epsilon flattens harder.  Endpoints are always
    /// kept; smoothness flags are preserved on surviving nodes.
    fn simplify(&mut self, epsilon: f32) {
        if self.pts.len() < 3 {
            return;
        }
        let pts = self.pts.clone();
        let mut keep = vec![false; pts.len()];
        keep[0] = true;
        *keep.last_mut().unwrap() = true;
        rdp_inner(&pts, 0, pts.len() - 1, epsilon, &mut keep);
        self.pts = pts
            .into_iter()
            .zip(keep.into_iter())
            .filter_map(|(p, k)| if k { Some(p) } else { None })
            .collect();
    }
}

fn rdp_inner(pts: &[CurveNode], start: usize, end: usize, epsilon: f32, keep: &mut [bool]) {
    if end <= start + 1 {
        return;
    }
    let p0 = pts[start];
    let p1 = pts[end];
    let mut max_d = 0.0_f32;
    let mut max_i = start;
    for i in (start + 1)..end {
        let d = perpendicular_distance(pts[i], p0, p1);
        if d > max_d {
            max_d = d;
            max_i = i;
        }
    }
    if max_d > epsilon {
        keep[max_i] = true;
        rdp_inner(pts, start, max_i, epsilon, keep);
        rdp_inner(pts, max_i, end, epsilon, keep);
    }
}

fn perpendicular_distance(p: CurveNode, a: CurveNode, b: CurveNode) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let denom = (dx * dx + dy * dy).sqrt().max(1e-9);
    let num = (dy * p.x - dx * p.y + b.x * a.y - b.y * a.x).abs();
    num / denom
}

struct GuiState {
    shared: SharedParams,
    resize: ResizeBridge,
    applied_size: (u32, u32),
    selected_preset: Option<usize>,
    preset_names: Vec<&'static str>,
    preview_a: Vec<f32>,
    preview_b: Vec<f32>,
    preview_for_preset: Option<usize>,
    edit_mode: bool,
    nodes: CurveNodes,
    dragging_node: Option<usize>,
    /// Currently selected node (for the smooth/sharp toggle button).
    selected_node: Option<usize>,
    /// True after the user has actively edited — used to decide whether
    /// to overwrite the curve when they pick a new preset.
    user_edited: bool,
    /// Undo / redo stacks for the curve editor. Each entry is a full
    /// CurveNodes snapshot (cheap — typically ≤ 32 nodes). Cap at 64
    /// entries to keep memory bounded.
    history: Vec<CurveNodes>,
    redo: Vec<CurveNodes>,
}

const HISTORY_CAP: usize = 64;

fn push_history(state: &mut GuiState) {
    state.history.push(state.nodes.clone());
    if state.history.len() > HISTORY_CAP {
        state.history.remove(0);
    }
    state.redo.clear();
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P,
    shared: SharedParams,
    resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Wave".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let state = GuiState {
        shared,
        resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(0),
        preset_names: PRESETS.iter().map(|p| p.name).collect(),
        preview_a: vec![0.0; WT_SIZE],
        preview_b: vec![0.0; WT_SIZE],
        preview_for_preset: None,
        edit_mode: false,
        history: Vec::with_capacity(HISTORY_CAP),
        redo: Vec::new(),
        nodes: CurveNodes::default(),
        dragging_node: None,
        selected_node: None,
        user_edited: false,
    };
    EguiWindow::open_parented(
        parent,
        settings,
        GraphicsConfig::default(),
        state,
        |ctx, _, _| core_gui::install_default_style(ctx),
        |ctx, queue, state| {
            let want = core_gui::read_bridge(&state.resize);
            if want != state.applied_size {
                queue.resize(PhySize::new(want.0, want.1));
                state.applied_size = want;
            }
            draw(ctx, state);
            ctx.request_repaint();
        },
    )
}

fn shape_label(i: u32) -> &'static str {
    match i {
        1 => "Triangle",
        2 => "Saw",
        3 => "Square",
        _ => "Sine",
    }
}

fn dest_label(i: u32) -> &'static str {
    match i {
        1 => "Pitch",
        2 => "WT Pos",
        _ => "Cutoff",
    }
}

fn refresh_preview(state: &mut GuiState) {
    let Some(i) = state.selected_preset else { return };
    if state.preview_for_preset == Some(i) {
        return;
    }
    let Some(preset) = PRESETS.get(i) else { return };
    let a = render_formula(preset.frame_a);
    let b = render_formula(preset.frame_b);
    state.preview_a.copy_from_slice(a.as_ref());
    state.preview_b.copy_from_slice(b.as_ref());
    state.preview_for_preset = Some(i);
    if !state.user_edited {
        state.nodes = CurveNodes::from_table(&state.preview_a, 24);
        state.selected_node = None;
    }
}

/// Draw the wave canvas inside `rect`. Returns true if the user changed the
/// curve (caller bakes + pushes new wavetable).
fn draw_wave_canvas(
    ui: &mut egui::Ui,
    state: &mut GuiState,
    wt_pos: f32,
    rect: egui::Rect,
) -> bool {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, core_gui::PANEL_BG);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
        egui::epaint::StrokeKind::Outside,
    );
    let centre_y = rect.center().y;
    painter.line_segment(
        [egui::pos2(rect.left(), centre_y), egui::pos2(rect.right(), centre_y)],
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
    );
    let half_h = rect.height() * 0.45;
    let to_screen =
        |x: f32, y: f32| -> egui::Pos2 { egui::pos2(rect.left() + x * rect.width(), centre_y - y * half_h) };
    let from_screen = |p: egui::Pos2| -> (f32, f32) {
        let x = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let y = ((centre_y - p.y) / half_h).clamp(-1.0, 1.0);
        (x, y)
    };

    if !state.edit_mode {
        const N: usize = 256;
        let mut prev: Option<egui::Pos2> = None;
        for i in 0..N {
            let t = i as f32 / N as f32;
            let idx = (t * WT_SIZE as f32) as usize;
            let a_s = state.preview_a.get(idx).copied().unwrap_or(0.0);
            let b_s = state.preview_b.get(idx).copied().unwrap_or(0.0);
            let v = a_s * (1.0 - wt_pos) + b_s * wt_pos;
            let pt = to_screen(t, v);
            if let Some(p) = prev {
                painter.line_segment([p, pt], egui::Stroke::new(1.5, core_gui::GREEN_BRIGHT));
            }
            prev = Some(pt);
        }
        return false;
    }

    let mut changed = false;
    let id = ui.id().with("wave_canvas");
    let response = ui.interact(rect, id, egui::Sense::click_and_drag());

    // Render the interpolated curve under the nodes.
    {
        const N: usize = 384;
        let mut prev: Option<egui::Pos2> = None;
        let rendered = state.nodes.render();
        for i in 0..N {
            let t = i as f32 / N as f32;
            let idx = (t * (rendered.len() - 1) as f32) as usize;
            let v = rendered[idx];
            let pt = to_screen(t, v);
            if let Some(p) = prev {
                painter.line_segment([p, pt], egui::Stroke::new(1.3, core_gui::GREEN));
            }
            prev = Some(pt);
        }
    }

    let pointer = response.interact_pointer_pos();
    let pointer_hover = response.hover_pos();
    let node_radius = 6.0_f32;

    // Hit-test for drag/select on press. Capture an undo snapshot at the
    // beginning of every potentially-mutating gesture (drag, add, delete).
    if response.drag_started() {
        push_history(state);
    }
    if response.drag_started() || response.clicked() {
        if let Some(p) = pointer_hover {
            let mut best: Option<(usize, f32)> = None;
            for (i, n) in state.nodes.pts.iter().enumerate() {
                let s = to_screen(n.x, n.y);
                let dist = ((s.x - p.x).powi(2) + (s.y - p.y).powi(2)).sqrt();
                if dist <= node_radius * 2.5 && best.map_or(true, |b| dist < b.1) {
                    best = Some((i, dist));
                }
            }
            if let Some((i, _)) = best {
                state.dragging_node = Some(i);
                state.selected_node = Some(i);
            } else if response.clicked() {
                state.selected_node = None;
            }
        }
    }

    if response.dragged() {
        if let (Some(i), Some(p)) = (state.dragging_node, pointer) {
            let last = state.nodes.pts.len().saturating_sub(1);
            if let Some(node) = state.nodes.pts.get_mut(i) {
                let (nx, ny) = from_screen(p);
                if i == 0 || i == last {
                    node.y = ny;
                } else {
                    node.x = nx;
                    node.y = ny;
                }
                changed = true;
            }
        }
    }
    if response.drag_stopped() {
        state.dragging_node = None;
        if changed {
            state.nodes.sort();
        }
    }

    let primary_double = response.double_clicked();
    let secondary_click = ui.input(|i| i.pointer.secondary_clicked());

    if primary_double {
        push_history(state);
        if let Some(p) = pointer_hover {
            let mut too_close = false;
            for n in &state.nodes.pts {
                let s = to_screen(n.x, n.y);
                if ((s.x - p.x).powi(2) + (s.y - p.y).powi(2)).sqrt() < node_radius * 2.0 {
                    too_close = true;
                    break;
                }
            }
            if !too_close {
                let (x, y) = from_screen(p);
                state.nodes.pts.push(CurveNode { x, y, smooth: false });
                state.nodes.sort();
                changed = true;
            }
        }
    }
    if secondary_click {
        push_history(state);
        if let Some(p) = pointer_hover {
            let mut victim: Option<usize> = None;
            for (i, n) in state.nodes.pts.iter().enumerate() {
                let s = to_screen(n.x, n.y);
                if ((s.x - p.x).powi(2) + (s.y - p.y).powi(2)).sqrt() < node_radius * 2.0 {
                    victim = Some(i);
                    break;
                }
            }
            if let Some(i) = victim {
                let last = state.nodes.pts.len().saturating_sub(1);
                if i != 0 && i != last && state.nodes.pts.len() > 2 {
                    state.nodes.pts.remove(i);
                    if state.selected_node == Some(i) {
                        state.selected_node = None;
                    }
                    changed = true;
                }
            }
        }
    }

    // Draw nodes — circles for smooth, squares for sharp; highlight selected.
    for (i, n) in state.nodes.pts.iter().enumerate() {
        let s = to_screen(n.x, n.y);
        let is_selected = state.selected_node == Some(i);
        let is_dragging = state.dragging_node == Some(i);
        let colour = if is_dragging {
            core_gui::GREEN_BRIGHT
        } else if is_selected {
            core_gui::GREEN_BRIGHT
        } else {
            core_gui::GREEN
        };
        if n.smooth {
            painter.circle_filled(s, node_radius, colour);
            painter.circle_stroke(s, node_radius, egui::Stroke::new(1.0, core_gui::PANEL_BG));
        } else {
            let r = egui::Rect::from_center_size(s, egui::vec2(node_radius * 2.0, node_radius * 2.0));
            painter.rect_filled(r, 1.0, colour);
            painter.rect_stroke(
                r,
                1.0,
                egui::Stroke::new(1.0, core_gui::PANEL_BG),
                egui::epaint::StrokeKind::Outside,
            );
        }
        if is_selected {
            painter.circle_stroke(s, node_radius + 3.0, egui::Stroke::new(1.0, core_gui::GREEN_BRIGHT));
        }
    }

    if changed {
        state.user_edited = true;
    }
    changed
}

fn draw(ctx: &egui::Context, state: &mut GuiState) {
    refresh_preview(state);

    // Undo / Redo — Ctrl/Cmd-Z / Ctrl-Shift-Z (or Ctrl-Y).
    let action = ctx.input(|i| {
        if i.modifiers.command && i.key_pressed(egui::Key::Z) {
            if i.modifiers.shift {
                Some(false) // redo
            } else {
                Some(true) // undo
            }
        } else if i.modifiers.command && i.key_pressed(egui::Key::Y) {
            Some(false)
        } else {
            None
        }
    });
    let mut history_changed = false;
    match action {
        Some(true) => {
            if let Some(prev) = state.history.pop() {
                state.redo.push(state.nodes.clone());
                state.nodes = prev;
                history_changed = true;
            }
        }
        Some(false) => {
            if let Some(next) = state.redo.pop() {
                state.history.push(state.nodes.clone());
                state.nodes = next;
                history_changed = true;
            }
        }
        None => {}
    }
    if history_changed {
        let table = state.nodes.render();
        let mip = mip_from_table(table.as_ref());
        push_custom_frame_a(&state.shared, mip);
    }

    egui::CentralPanel::default().show(ctx, |ui| {
        if let Some(i) = core_gui::top_bar(
            ui,
            "SuperDuper Wave",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "wave_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            apply_preset(&state.shared, i);
            state.preview_for_preset = None;
            state.user_edited = false;
            state.selected_node = None;
        }

        core_gui::ab_init_bar(
            ui,
            &state.shared.ab_snapshot,
            &state.shared.params,
            PARAMS,
            &state.shared.dirty_params,
        );
        let (scope_rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 80.0), egui::Sense::hover());
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, scope_rect, 48_000.0);
        let active = state.shared.active_voices.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            ui.label(format!("Voices: {active} / {}", crate::VOICE_COUNT));
            ui.add_space(8.0);
            ui.checkbox(&mut state.edit_mode, "edit curve");
            ui.add_space(8.0);
            if ui.button("reset to preset").clicked() {
                state.user_edited = false;
                state.preview_for_preset = None;
                state.selected_node = None;
                if let Some(i) = state.selected_preset {
                    apply_preset(&state.shared, i);
                }
            }
        });

        let mut curve_changed = false;
        if state.edit_mode {
            ui.horizontal(|ui| {
                if ui.button("smooth all").clicked() {
                    for n in state.nodes.pts.iter_mut() {
                        n.smooth = true;
                    }
                    state.user_edited = true;
                    curve_changed = true;
                }
                if ui.button("sharpen all").clicked() {
                    for n in state.nodes.pts.iter_mut() {
                        n.smooth = false;
                    }
                    state.user_edited = true;
                    curve_changed = true;
                }
                if ui.button("simplify").on_hover_text("drop redundant nodes (RDP, ε=0.03)").clicked() {
                    state.nodes.simplify(0.03);
                    state.selected_node = None;
                    state.user_edited = true;
                    curve_changed = true;
                }
                if ui.button("simplify hard").on_hover_text("aggressive (RDP, ε=0.08)").clicked() {
                    state.nodes.simplify(0.08);
                    state.selected_node = None;
                    state.user_edited = true;
                    curve_changed = true;
                }
                ui.weak(format!("{} nodes", state.nodes.pts.len()));
            });
            ui.horizontal(|ui| {
                if let Some(idx) = state.selected_node {
                    if let Some(node) = state.nodes.pts.get_mut(idx) {
                        let label = if node.smooth { "make sharp" } else { "make smooth" };
                        if ui.button(label).clicked() {
                            node.smooth = !node.smooth;
                            state.user_edited = true;
                            curve_changed = true;
                        }
                        ui.weak(format!(
                            "node {idx} · x={:.3} y={:+.3} · {}",
                            node.x,
                            node.y,
                            if node.smooth { "smooth" } else { "sharp" }
                        ));
                    }
                } else {
                    ui.weak("click a node to select  ·  dbl-click add  ·  rmb delete  ·  drag move");
                }
            });
        }
        ui.add_space(4.0);

        let wt_pos = state.shared.params[P_WT_POS].load(Ordering::Relaxed).clamp(0.0, 1.0);
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 200.0), egui::Sense::hover());
        if draw_wave_canvas(ui, state, wt_pos, rect) {
            curve_changed = true;
        }
        if curve_changed {
            // Build the full mip pyramid off the GUI thread — ~10 FFTs of
            // 2048 samples is fast enough to do inline on each edit.
            let table = state.nodes.render();
            let mip = mip_from_table(table.as_ref());
            push_custom_frame_a(&state.shared, mip);
        }
        ui.add_space(6.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Oscillator", |ui| {
                core_gui::dirty_param_row(ui, &state.shared.params[P_WT_POS], &PARAMS[P_WT_POS], &state.shared.dirty_params[P_WT_POS]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_UNISON], &PARAMS[P_UNISON], &state.shared.dirty_params[P_UNISON]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_DETUNE], &PARAMS[P_DETUNE], &state.shared.dirty_params[P_DETUNE]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_SUB], &PARAMS[P_SUB], &state.shared.dirty_params[P_SUB]);
            });
            core_gui::section(ui, "Mix", |ui| {
                core_gui::dirty_param_row(ui, &state.shared.params[P_NOISE], &PARAMS[P_NOISE], &state.shared.dirty_params[P_NOISE]);
            });
            core_gui::section(ui, "Filter", |ui| {
                core_gui::dirty_param_row(ui, &state.shared.params[P_CUTOFF], &PARAMS[P_CUTOFF], &state.shared.dirty_params[P_CUTOFF]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_RESONANCE], &PARAMS[P_RESONANCE], &state.shared.dirty_params[P_RESONANCE]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_FILTER_MODE], &PARAMS[P_FILTER_MODE], &state.shared.dirty_params[P_FILTER_MODE]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_DRIVE], &PARAMS[P_DRIVE], &state.shared.dirty_params[P_DRIVE]);
            });
            core_gui::section(ui, "Filter Envelope", |ui| {
                core_gui::dirty_param_row(ui, &state.shared.params[P_FENV_AMOUNT], &PARAMS[P_FENV_AMOUNT], &state.shared.dirty_params[P_FENV_AMOUNT]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_FENV_A], &PARAMS[P_FENV_A], &state.shared.dirty_params[P_FENV_A]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_FENV_D], &PARAMS[P_FENV_D], &state.shared.dirty_params[P_FENV_D]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_FENV_S], &PARAMS[P_FENV_S], &state.shared.dirty_params[P_FENV_S]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_FENV_R], &PARAMS[P_FENV_R], &state.shared.dirty_params[P_FENV_R]);
            });
            core_gui::section(ui, "LFO 1", |ui| {
                let shape_idx = state.shared.params[P_LFO_SHAPE]
                    .load(Ordering::Relaxed) as u32;
                let dest_idx = state.shared.params[P_LFO_DEST]
                    .load(Ordering::Relaxed) as u32;
                ui.weak(format!(
                    "shape: {}  ·  dest: {}",
                    shape_label(shape_idx),
                    dest_label(dest_idx)
                ));
                core_gui::dirty_param_row(ui, &state.shared.params[P_LFO_SHAPE], &PARAMS[P_LFO_SHAPE], &state.shared.dirty_params[P_LFO_SHAPE]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_LFO_DEST], &PARAMS[P_LFO_DEST], &state.shared.dirty_params[P_LFO_DEST]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_LFO_RATE], &PARAMS[P_LFO_RATE], &state.shared.dirty_params[P_LFO_RATE]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_LFO_DEPTH], &PARAMS[P_LFO_DEPTH], &state.shared.dirty_params[P_LFO_DEPTH]);
            });
            core_gui::section(ui, "Amp Envelope", |ui| {
                core_gui::dirty_param_row(ui, &state.shared.params[P_ATTACK], &PARAMS[P_ATTACK], &state.shared.dirty_params[P_ATTACK]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_DECAY], &PARAMS[P_DECAY], &state.shared.dirty_params[P_DECAY]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_SUSTAIN], &PARAMS[P_SUSTAIN], &state.shared.dirty_params[P_SUSTAIN]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_RELEASE], &PARAMS[P_RELEASE], &state.shared.dirty_params[P_RELEASE]);
            });
            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT]);
                // Anti-alias as a checkbox so it's obvious + accessible
                // without typing — still backed by the param so the host
                // can automate and remember it.
                let mut aa_on = state.shared.params[P_ANTIALIAS]
                    .load(Ordering::Relaxed) >= 0.5;
                if ui
                    .checkbox(&mut aa_on, "Anti-Alias (mip-mapped wavetable)")
                    .on_hover_text("Off = raw read (audible aliasing on high notes). On = per-voice band-limited mip pyramid.")
                    .changed()
                {
                    state.shared.params[P_ANTIALIAS]
                        .store(if aa_on { 1.0 } else { 0.0 }, Ordering::Relaxed);
                }
            });
        });
    });
}
