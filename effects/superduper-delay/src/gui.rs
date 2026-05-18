//! GUI for SuperDuper Delay.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{P_DRIVE, P_FEEDBACK, P_MIX, P_MODE, P_TIME, P_TIME_DIV, P_TIME_SYNC, P_TONE, P_WIDTH, PARAMS, SharedParams};

pub const DEFAULT_WIDTH: u32 = 460;
pub const DEFAULT_HEIGHT: u32 = 380;
pub const MIN_WIDTH: u32 = 360;
pub const MIN_HEIGHT: u32 = 320;
pub const MAX_WIDTH: u32 = 1200;
pub const MAX_HEIGHT: u32 = 900;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

const MODE_NAMES: [&str; 3] = ["Stereo", "Ping-Pong", "Slap (Haas)"];

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
        title: "SuperDuper Delay".to_string(),
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
    use std::sync::atomic::Ordering;
    egui::CentralPanel::default().show(ctx, |ui| {
        if let Some(i) = core_gui::top_bar(
            ui,
            "SuperDuper Delay",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "delay_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
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
            core_gui::section(ui, "Time", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_TIME], &PARAMS[P_TIME], &state.shared.dirty_params[P_TIME], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_TIME);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_WIDTH], &PARAMS[P_WIDTH], &state.shared.dirty_params[P_WIDTH], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_WIDTH);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_TIME_SYNC], &PARAMS[P_TIME_SYNC], &state.shared.dirty_params[P_TIME_SYNC], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_TIME_SYNC);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_TIME_DIV], &PARAMS[P_TIME_DIV], &state.shared.dirty_params[P_TIME_DIV], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_TIME_DIV);
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [90.0, 18.0],
                        egui::Label::new(
                            egui::RichText::new("Mode").color(core_gui::GREEN).monospace(),
                        ),
                    );
                    let cur = state.shared.params[P_MODE].load(Ordering::Relaxed).round() as usize;
                    let cur = cur.min(MODE_NAMES.len() - 1);
                    egui::ComboBox::from_id_salt("delay_mode_combo")
                        .selected_text(MODE_NAMES[cur])
                        .width(140.0)
                        .show_ui(ui, |ui| {
                            for (i, name) in MODE_NAMES.iter().enumerate() {
                                if ui.selectable_label(cur == i, *name).clicked() {
                                    state.shared.params[P_MODE]
                                        .store(i as f32, Ordering::Relaxed);
                                }
                            }
                        });
                });
            });

            core_gui::section(ui, "Feedback", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_FEEDBACK], &PARAMS[P_FEEDBACK], &state.shared.dirty_params[P_FEEDBACK], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_FEEDBACK);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_TONE], &PARAMS[P_TONE], &state.shared.dirty_params[P_TONE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_TONE);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_DRIVE], &PARAMS[P_DRIVE], &state.shared.dirty_params[P_DRIVE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_DRIVE);
            });

            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MIX], &PARAMS[P_MIX], &state.shared.dirty_params[P_MIX], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MIX);
            });
        });
    });
}
