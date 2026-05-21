//! SuperDuper Mid/Side — compact mastering UI.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{PARAMS, P_MID, P_MODE, P_OUTPUT, P_SIDE, P_WIDTH, SharedParams};

pub const DEFAULT_WIDTH: u32 = 460;
pub const DEFAULT_HEIGHT: u32 = 340;
pub const MIN_WIDTH: u32 = 360;
pub const MIN_HEIGHT: u32 = 260;
pub const MAX_WIDTH: u32 = 1000;
pub const MAX_HEIGHT: u32 = 700;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

const MODE_NAMES: [&str; 3] = ["Width", "Encode →", "← Decode"];

struct GuiState {
    shared: SharedParams,
    resize: ResizeBridge,
    applied_size: (u32, u32),
    selected_preset: Option<usize>,
    preset_names: Vec<&'static str>,
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P,
    shared: SharedParams,
    resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Mid/Side".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let initial_preset_idx = (shared.active_preset.load(Ordering::Relaxed) as usize)
        .min(PRESETS.len().saturating_sub(1));
    let state = GuiState {
        shared,
        resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(initial_preset_idx),
        preset_names: PRESETS.iter().map(|p| p.name).collect(),
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
        if let Some(i) = core_gui::top_bar(
            ui,
            "SuperDuper Mid/Side",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "midside_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            if let Some(preset) = PRESETS.get(i) {
                crate::presets::apply(&state.shared, preset);
                state.shared.active_preset.store(i as u32, Ordering::Relaxed);
            }
        }
        core_gui::ab_init_bar(
            ui,
            &state.shared.ab_snapshot,
            &state.shared.params,
            PARAMS,
            &state.shared.dirty_params,
        );

        let (scope_rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 50.0), egui::Sense::hover());
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, scope_rect, 48_000.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Mode", |ui| {
                core_gui::dirty_choice_row_g(
                    ui,
                    &state.shared.params[P_MODE],
                    &PARAMS[P_MODE],
                    &MODE_NAMES,
                    &state.shared.dirty_params[P_MODE],
                    core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end },
                    P_MODE,
                );
                let cur = state.shared.params[P_MODE].load(Ordering::Relaxed).round() as i32;
                ui.label(
                    egui::RichText::new(match cur {
                        1 => "L/R in → L=Mid, R=Side out (insert before mastering chain)",
                        2 => "L=Mid, R=Side in → L/R out (insert after mastering chain)",
                        _ => "Width / per-band gain on a stereo signal in-place",
                    })
                    .color(core_gui::GREEN_DIM)
                    .monospace()
                    .small(),
                );
            });

            core_gui::section(ui, "Width / Gains", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_WIDTH], &PARAMS[P_WIDTH], &state.shared.dirty_params[P_WIDTH], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_WIDTH);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MID], &PARAMS[P_MID], &state.shared.dirty_params[P_MID], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MID);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_SIDE], &PARAMS[P_SIDE], &state.shared.dirty_params[P_SIDE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_SIDE);
            });

            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_OUTPUT);
            });
        });
    });
}
