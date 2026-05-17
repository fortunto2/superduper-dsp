//! GUI for SuperDuper Limiter — knobs + gain-reduction meter.

use std::sync::atomic::Ordering;

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{P_CEILING, P_INPUT, P_LOOKAHEAD, P_RELEASE, P_TRUE_PEAK, PARAMS, SharedParams};

pub const DEFAULT_WIDTH: u32 = 460;
pub const DEFAULT_HEIGHT: u32 = 400;
pub const MIN_WIDTH: u32 = 360;
pub const MIN_HEIGHT: u32 = 320;
pub const MAX_WIDTH: u32 = 1200;
pub const MAX_HEIGHT: u32 = 900;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

struct GuiState {
    shared: SharedParams,
    resize: ResizeBridge,
    applied_size: (u32, u32),
    selected_preset: Option<usize>,
    preset_names: Vec<&'static str>,
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P, shared: SharedParams, resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Limiter".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let state = GuiState {
        shared, resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(0),
        preset_names: PRESETS.iter().map(|p| p.name).collect(),
    };
    EguiWindow::open_parented(
        parent, settings, GraphicsConfig::default(), state,
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
        if let Some(i) = core_gui::top_bar(
            ui, "SuperDuper Limiter",
            env!("SDSP_BUILD_NUM"), env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "limiter_preset_combo", &state.preset_names, &mut state.selected_preset,
        ) {
            if let Some(preset) = PRESETS.get(i) {
                crate::presets::apply(&state.shared, preset);
            }
        }
        draw_gr_meter(ui, &state.shared);
        ui.add_space(4.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Levels", |ui| {
                core_gui::param_row(ui, &state.shared.params[P_INPUT], &PARAMS[P_INPUT]);
                core_gui::param_row(ui, &state.shared.params[P_CEILING], &PARAMS[P_CEILING]);
            });
            core_gui::section(ui, "Envelope", |ui| {
                core_gui::param_row(ui, &state.shared.params[P_RELEASE], &PARAMS[P_RELEASE]);
                core_gui::param_row(ui, &state.shared.params[P_LOOKAHEAD], &PARAMS[P_LOOKAHEAD]);
            });
            core_gui::section(ui, "Detection", |ui| {
                ui.horizontal(|ui| {
                    let on = state.shared.params[P_TRUE_PEAK].load(Ordering::Relaxed) > 0.5;
                    let label = if on { "[X] true-peak (4×)" } else { "[ ] true-peak (4×)" };
                    if ui.selectable_label(on, egui::RichText::new(label)
                        .color(core_gui::GREEN).monospace()).clicked() {
                        state.shared.params[P_TRUE_PEAK]
                            .store(if on { 0.0 } else { 1.0 }, Ordering::Relaxed);
                    }
                });
            });
        });
    });
}

fn draw_gr_meter(ui: &mut egui::Ui, shared: &crate::SharedParamsInner) {
    let gr_db = shared.gain_reduction_db.load(Ordering::Relaxed);
    let display = gr_db.max(-18.0).min(0.0);
    let frac = (-display / 18.0).clamp(0.0, 1.0);
    let desired = egui::vec2(ui.available_width().min(420.0), 22.0);
    let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, core_gui::PANEL_BG);
    painter.rect_stroke(rect, 0.0,
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT), egui::StrokeKind::Inside);
    let fill_w = rect.width() * frac;
    if fill_w > 1.0 {
        let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height()));
        let bright = (0.4 + frac * 0.6).min(1.0);
        let r = (core_gui::GREEN_BRIGHT.r() as f32 * bright) as u8;
        let g = (core_gui::GREEN_BRIGHT.g() as f32 * bright) as u8;
        let b = (core_gui::GREEN_BRIGHT.b() as f32 * bright) as u8;
        painter.rect_filled(fill_rect, 0.0, egui::Color32::from_rgb(r, g, b));
    }
    for db in (1..=6).map(|i| i * 3) {
        let x = rect.left() + rect.width() * (db as f32 / 18.0);
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.top() + 4.0)],
            egui::Stroke::new(1.0, core_gui::GREEN_DIM),
        );
    }
    painter.text(
        egui::pos2(rect.right() - 6.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("{:.1} dB GR", display),
        egui::FontId::monospace(11.0),
        core_gui::GREEN_BRIGHT,
    );
}
