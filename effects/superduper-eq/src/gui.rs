//! GUI for SuperDuper EQ.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{P_HIGH_FREQ, P_HIGH_GAIN, P_HP, P_LOW_FREQ, P_LOW_GAIN, P_LP, P_MID_FREQ, P_MID_GAIN,
            P_MID_Q, P_OUTPUT, PARAMS, SharedParams};

pub const DEFAULT_WIDTH: u32 = 520;
pub const DEFAULT_HEIGHT: u32 = 500;
pub const MIN_WIDTH: u32 = 400;
pub const MIN_HEIGHT: u32 = 380;
pub const MAX_WIDTH: u32 = 1400;
pub const MAX_HEIGHT: u32 = 1100;

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
        title: "SuperDuper EQ".to_string(),
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
            ui, "SuperDuper EQ",
            env!("SDSP_BUILD_NUM"), env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "eq_preset_combo", &state.preset_names, &mut state.selected_preset,
        ) {
            if let Some(preset) = PRESETS.get(i) {
                crate::presets::apply(&state.shared, preset);
            }

        core_gui::ab_init_bar(
            ui,
            &state.shared.ab_snapshot,
            &state.shared.params,
            PARAMS,
            &state.shared.dirty_params,
        );
        let (sdsp_scope_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 60.0),
            egui::Sense::hover(),
        );
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, sdsp_scope_rect, 48_000.0);
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Filters", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_HP], &PARAMS[P_HP], &state.shared.dirty_params[P_HP], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_HP);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LP], &PARAMS[P_LP], &state.shared.dirty_params[P_LP], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LP);
            });
            core_gui::section(ui, "Low Shelf", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LOW_FREQ], &PARAMS[P_LOW_FREQ], &state.shared.dirty_params[P_LOW_FREQ], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LOW_FREQ);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LOW_GAIN], &PARAMS[P_LOW_GAIN], &state.shared.dirty_params[P_LOW_GAIN], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LOW_GAIN);
            });
            core_gui::section(ui, "Mid Peak", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MID_FREQ], &PARAMS[P_MID_FREQ], &state.shared.dirty_params[P_MID_FREQ], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MID_FREQ);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MID_GAIN], &PARAMS[P_MID_GAIN], &state.shared.dirty_params[P_MID_GAIN], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MID_GAIN);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MID_Q], &PARAMS[P_MID_Q], &state.shared.dirty_params[P_MID_Q], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MID_Q);
            });
            core_gui::section(ui, "High Shelf", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_HIGH_FREQ], &PARAMS[P_HIGH_FREQ], &state.shared.dirty_params[P_HIGH_FREQ], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_HIGH_FREQ);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_HIGH_GAIN], &PARAMS[P_HIGH_GAIN], &state.shared.dirty_params[P_HIGH_GAIN], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_HIGH_GAIN);
            });
            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_OUTPUT);
            });
        });
    });
}
