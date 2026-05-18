use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{
    PARAMS, P_CLK_AMT, P_CLK_FLOOR, P_CLK_SENS, P_ESS_AMT, P_ESS_FREQ, P_ESS_RANGE, P_ESS_THR,
    P_MIX, P_OUTPUT, SharedParams,
};

pub const DEFAULT_WIDTH: u32 = 560;
pub const DEFAULT_HEIGHT: u32 = 540;
pub const MIN_WIDTH: u32 = 420;
pub const MIN_HEIGHT: u32 = 400;
pub const MAX_WIDTH: u32 = 1400;
pub const MAX_HEIGHT: u32 = 1200;

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
    parent: &P,
    shared: SharedParams,
    resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Vocal".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let state = GuiState {
        shared,
        resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(1),
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
            "SuperDuper Vocal",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "vocal_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            if let Some(preset) = PRESETS.get(i) {
                crate::presets::apply(&state.shared, preset);
            }
        }

        // Stage meters: two GR bars side by side.
        let ess_gr = state.shared.ess_gr_db.load(Ordering::Relaxed);
        let click_gr = state.shared.click_gr_db.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("ess GR {ess_gr:>5.1} dB"))
                    .color(core_gui::GREEN_BRIGHT)
                    .monospace(),
            );
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new(format!("clk GR {click_gr:>5.1} dB"))
                    .color(core_gui::GREEN_BRIGHT)
                    .monospace(),
            );
        });
        ui.add_space(4.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "De-Esser", |ui| {
                core_gui::dirty_param_row(ui, &state.shared.params[P_ESS_THR], &PARAMS[P_ESS_THR], &state.shared.dirty_params[P_ESS_THR]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_ESS_FREQ], &PARAMS[P_ESS_FREQ], &state.shared.dirty_params[P_ESS_FREQ]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_ESS_AMT], &PARAMS[P_ESS_AMT], &state.shared.dirty_params[P_ESS_AMT]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_ESS_RANGE], &PARAMS[P_ESS_RANGE], &state.shared.dirty_params[P_ESS_RANGE]);
            });
            core_gui::section(ui, "De-Clicker", |ui| {
                core_gui::dirty_param_row(ui, &state.shared.params[P_CLK_SENS], &PARAMS[P_CLK_SENS], &state.shared.dirty_params[P_CLK_SENS]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_CLK_AMT], &PARAMS[P_CLK_AMT], &state.shared.dirty_params[P_CLK_AMT]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_CLK_FLOOR], &PARAMS[P_CLK_FLOOR], &state.shared.dirty_params[P_CLK_FLOOR]);
            });
            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_MIX], &PARAMS[P_MIX], &state.shared.dirty_params[P_MIX]);
            });
        });
    });
}
