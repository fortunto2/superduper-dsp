//! egui_baseview GUI for SuperDuper Reverb.
//!
//! Per-plugin specifics only: window title, sizes, section layout, preset
//! list. Shared style/layout primitives (font sizes, section header, param
//! row, preset combo) come from `superduper_synth_core::gui` so every new
//! effect picks up the same look automatically.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{P_DAMP, P_DECAY, P_DUCK_AMOUNT, P_DUCK_ATTACK, P_DUCK_RELEASE, P_FREEZE, P_MIX,
            P_MOD, P_PD_DIV, P_PD_SYNC, P_PREDELAY, P_SIZE, P_WIDTH, PARAMS, SharedParams};

// Compact-by-default. Fits REAPER's FX window without forcing the user to
// drag-resize. Mono 12 px + tight spacing makes this enough for 10 params.
pub const DEFAULT_WIDTH: u32 = 480;
pub const DEFAULT_HEIGHT: u32 = 480;
pub const MIN_WIDTH: u32 = 360;
pub const MIN_HEIGHT: u32 = 380;
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
        title: "SuperDuper Reverb".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let initial_preset_idx = (shared.active_preset.load(std::sync::atomic::Ordering::Relaxed) as usize)
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
        |ctx: &egui::Context, _queue, _state: &mut GuiState| {
            core_gui::install_default_style(ctx)
        },
        |ctx: &egui::Context, queue, state: &mut GuiState| {
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
            "SuperDuper Reverb",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "reverb_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            apply_preset(state, i);
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
        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Space", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_SIZE], &PARAMS[P_SIZE], &state.shared.dirty_params[P_SIZE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_SIZE);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_DECAY], &PARAMS[P_DECAY], &state.shared.dirty_params[P_DECAY], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_DECAY);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_DAMP], &PARAMS[P_DAMP], &state.shared.dirty_params[P_DAMP], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_DAMP);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_PREDELAY], &PARAMS[P_PREDELAY], &state.shared.dirty_params[P_PREDELAY], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_PREDELAY);
                core_gui::dirty_toggle_row_g(ui, &state.shared.params[P_PD_SYNC], &PARAMS[P_PD_SYNC], &state.shared.dirty_params[P_PD_SYNC], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_PD_SYNC);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_PD_DIV], &PARAMS[P_PD_DIV], &state.shared.dirty_params[P_PD_DIV], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_PD_DIV);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MOD], &PARAMS[P_MOD], &state.shared.dirty_params[P_MOD], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MOD);
                core_gui::dirty_toggle_row_g(ui, &state.shared.params[P_FREEZE], &PARAMS[P_FREEZE], &state.shared.dirty_params[P_FREEZE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_FREEZE);
            });
            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_WIDTH], &PARAMS[P_WIDTH], &state.shared.dirty_params[P_WIDTH], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_WIDTH);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MIX], &PARAMS[P_MIX], &state.shared.dirty_params[P_MIX], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MIX);
            });
            core_gui::section(ui, "Ducking", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_DUCK_AMOUNT], &PARAMS[P_DUCK_AMOUNT], &state.shared.dirty_params[P_DUCK_AMOUNT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_DUCK_AMOUNT);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_DUCK_ATTACK], &PARAMS[P_DUCK_ATTACK], &state.shared.dirty_params[P_DUCK_ATTACK], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_DUCK_ATTACK);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_DUCK_RELEASE], &PARAMS[P_DUCK_RELEASE], &state.shared.dirty_params[P_DUCK_RELEASE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_DUCK_RELEASE);
            });
        });
    });
}

fn apply_preset(state: &mut GuiState, index: usize) {
    if let Some(preset) = PRESETS.get(index) {
        crate::presets::apply(&state.shared, preset);
        state.shared.active_preset.store(index as u32, std::sync::atomic::Ordering::Relaxed);
    }
}

