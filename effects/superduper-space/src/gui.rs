//! The room: a top-down map with the layout's speakers around the listener
//! and the source as a dot you drag; the speaker dots glow with the gains
//! the last block used, so the placement reads as sound, not geometry.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::dsp::{Layout, LAYOUT_NAMES};
use crate::presets::PRESETS;
use crate::{PARAMS, P_DISTANCE, P_LAYOUT, P_LFE, P_MONITOR, P_OUTPUT, P_SPREAD, P_WIDTH, P_X, P_Y, P_Z, SharedParams};

pub const DEFAULT_WIDTH: u32 = 520;
pub const DEFAULT_HEIGHT: u32 = 620;
pub const MIN_WIDTH: u32 = 380;
pub const MIN_HEIGHT: u32 = 420;
pub const MAX_WIDTH: u32 = 1200;
pub const MAX_HEIGHT: u32 = 1200;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

const MONITOR_NAMES: [&str; 2] = ["Off", "Stereo fold"];

struct GuiState {
    shared: SharedParams,
    resize: ResizeBridge,
    applied_size: (u32, u32),
    selected_preset: Option<usize>,
    preset_names: Vec<&'static str>,
    dragging: bool,
}

pub fn open_window<P: HasRawWindowHandle>(parent: &P, shared: SharedParams, resize: ResizeBridge) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Space".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let initial_preset_idx = (shared.active_preset.load(Ordering::Relaxed) as usize).min(PRESETS.len().saturating_sub(1));
    let state = GuiState {
        shared,
        resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(initial_preset_idx),
        preset_names: PRESETS.iter().map(|p| p.name).collect(),
        dragging: false,
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

fn draw(ctx: &egui::Context, state: &mut GuiState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        if let Some(i) = core_gui::top_bar_mix(
            &state.shared.scope,
            ui,
            "SuperDuper Space",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "space_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            if let Some(preset) = PRESETS.get(i) {
                crate::presets::apply(&state.shared, preset);
                state.shared.active_preset.store(i as u32, Ordering::Relaxed);
            }
        }
        core_gui::ab_init_bar(ui, &state.shared.ab_snapshot, &state.shared.params, PARAMS, &state.shared.dirty_params);

        let gestures = core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end };
        core_gui::dirty_choice_row_g(ui, &state.shared.params[P_LAYOUT], &PARAMS[P_LAYOUT], &LAYOUT_NAMES, &state.shared.dirty_params[P_LAYOUT], gestures, P_LAYOUT);

        let side = ui.available_width().min(ui.available_height() - 230.0).max(160.0);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click_and_drag());
        draw_map(ui, rect, &resp, &state.shared, &mut state.dragging);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Place", |ui| {
                for p in [P_Z, P_SPREAD, P_DISTANCE, P_WIDTH] {
                    core_gui::dirty_param_row_g(ui, &state.shared.params[p], &PARAMS[p], &state.shared.dirty_params[p], gestures, p);
                }
            });
            core_gui::section(ui, "Out", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LFE], &PARAMS[P_LFE], &state.shared.dirty_params[P_LFE], gestures, P_LFE);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT], gestures, P_OUTPUT);
                core_gui::dirty_choice_row_g(ui, &state.shared.params[P_MONITOR], &PARAMS[P_MONITOR], &MONITOR_NAMES, &state.shared.dirty_params[P_MONITOR], gestures, P_MONITOR);
                ui.label(egui::RichText::new("Monitor folds every speaker onto channels 1–2 — hear the placement on a stereo track; off for a surround bus.").color(core_gui::GREEN_DIM).small());
            });
        });
    });
}

/// The map: listener in the middle, front up. Speakers sit on a ring at
/// their azimuth (height ones on a smaller ring), lit by the gain they
/// carry; the source dot follows X/Y and drags.
fn draw_map(ui: &mut egui::Ui, rect: egui::Rect, resp: &egui::Response, shared: &SharedParams, dragging: &mut bool) {
    let painter = ui.painter_at(rect);
    let c = rect.center();
    let radius = rect.width().min(rect.height()) * 0.42;
    painter.rect_filled(rect, 6.0, egui::Color32::from_rgb(10, 14, 12));
    painter.circle_stroke(c, radius, egui::Stroke::new(1.0_f32, core_gui::GREEN_FAINT));
    painter.circle_stroke(c, radius * 0.6, egui::Stroke::new(0.5_f32, core_gui::GREEN_FAINT));
    painter.line_segment([egui::pos2(c.x, rect.top() + 4.0), egui::pos2(c.x, rect.bottom() - 4.0)], egui::Stroke::new(0.5_f32, core_gui::GREEN_FAINT));
    painter.line_segment([egui::pos2(rect.left() + 4.0, c.y), egui::pos2(rect.right() - 4.0, c.y)], egui::Stroke::new(0.5_f32, core_gui::GREEN_FAINT));
    painter.text(egui::pos2(c.x, rect.top() + 10.0), egui::Align2::CENTER_CENTER, "front", egui::FontId::monospace(10.0), core_gui::GREEN_DIM);

    let layout = Layout::from_param(shared.params[P_LAYOUT].load(Ordering::Relaxed));
    for (ch, name, az, height) in layout.speakers() {
        let r = if height { radius * 0.6 } else { radius };
        let a = az.to_radians();
        let p = if name == "LFE" { egui::pos2(rect.left() + 18.0, rect.bottom() - 14.0) } else { egui::pos2(c.x + r * a.sin(), c.y - r * a.cos()) };
        let g = shared.live_gain[ch].load(Ordering::Relaxed).clamp(0.0, 1.0);
        let lit = egui::Color32::from_rgb((40.0 + 200.0 * g) as u8, (90.0 + 160.0 * g) as u8, (50.0 + 60.0 * g) as u8);
        painter.circle_filled(p, 5.0 + 6.0 * g, lit);
        painter.text(p + egui::vec2(0.0, -12.0), egui::Align2::CENTER_CENTER, name, egui::FontId::monospace(10.0), core_gui::GREEN_DIM);
    }

    let (x, y) = (shared.params[P_X].load(Ordering::Relaxed), shared.params[P_Y].load(Ordering::Relaxed));
    let src = egui::pos2(c.x + x * radius, c.y - y * radius);
    let z = shared.params[P_Z].load(Ordering::Relaxed);
    painter.circle_filled(src, 7.0 + 4.0 * z, core_gui::GREEN_BRIGHT);
    painter.circle_stroke(src, 7.0 + 4.0 * z, egui::Stroke::new(1.0_f32, egui::Color32::WHITE));

    // Drag: the pointer position becomes X/Y, clamped to the map; one
    // gesture per drag so the host records a single automation move.
    if resp.drag_started() {
        *dragging = true;
        for p in [P_X, P_Y] { shared.gesture_begin[p].store(true, Ordering::Relaxed); }
    }
    if *dragging {
        if let Some(pos) = resp.interact_pointer_pos() {
            let nx = ((pos.x - c.x) / radius).clamp(-1.0, 1.0);
            let ny = ((c.y - pos.y) / radius).clamp(-1.0, 1.0);
            shared.params[P_X].store(nx, Ordering::Relaxed);
            shared.params[P_Y].store(ny, Ordering::Relaxed);
            shared.dirty_params[P_X].store(true, Ordering::Relaxed);
            shared.dirty_params[P_Y].store(true, Ordering::Relaxed);
        }
    }
    if resp.drag_stopped() && *dragging {
        *dragging = false;
        for p in [P_X, P_Y] { shared.gesture_end[p].store(true, Ordering::Relaxed); }
    }
    let az = crate::dsp::azimuth_deg(x, y);
    painter.text(egui::pos2(rect.right() - 6.0, rect.bottom() - 8.0), egui::Align2::RIGHT_CENTER, format!("x {x:+.2}  y {y:+.2}  {az:+.0}°"), egui::FontId::monospace(10.0), core_gui::GREEN_DIM);
}
