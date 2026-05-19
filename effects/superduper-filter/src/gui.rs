//! SuperDuper Filter GUI — multi-mode + drive + LFO + env follow.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::dsp_blocks::sync_division_label;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{
    cutoff_units_to_hz, PARAMS, P_CUTOFF, P_DRIVE, P_DRV_TYPE, P_ENV_ATK, P_ENV_DPT, P_ENV_REL,
    P_LFO_DIV, P_LFO_DPT, P_LFO_RATE, P_LFO_SHP, P_LFO_SYNC, P_MIX, P_OUTPUT, P_RESO, P_TYPE,
    SharedParams,
};

pub const DEFAULT_WIDTH: u32 = 560;
pub const DEFAULT_HEIGHT: u32 = 600;
pub const MIN_WIDTH: u32 = 460;
pub const MIN_HEIGHT: u32 = 480;
pub const MAX_WIDTH: u32 = 1400;
pub const MAX_HEIGHT: u32 = 1200;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

const TYPE_NAMES: [&str; 4] = ["LP", "HP", "BP", "Notch"];
const DRV_NAMES: [&str; 4] = ["Off", "Tanh", "Tape", "Tube"];
const SHAPE_NAMES: [&str; 4] = ["Sine", "Tri", "Saw", "Square"];

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
        title: "SuperDuper Filter".to_string(),
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
    egui::CentralPanel::default().show(ctx, |ui| {
        if let Some(i) = core_gui::top_bar(
            ui,
            "SuperDuper Filter",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "filter_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            if let Some(preset) = PRESETS.get(i) {
                crate::presets::apply(&state.shared, preset);
            }
        }
        core_gui::ab_init_bar(
            ui,
            &state.shared.ab_snapshot,
            &state.shared.params,
            PARAMS,
            &state.shared.dirty_params,
        );

        let (scope_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 50.0),
            egui::Sense::hover(),
        );
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, scope_rect, 48_000.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Filter", |ui| {
                // Type selector — clearer as labelled buttons than a 0..3 slider.
                let cur_type = state.shared.params[P_TYPE].load(Ordering::Relaxed).round() as i32;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Type").color(core_gui::GREEN).monospace());
                    for (i, name) in TYPE_NAMES.iter().enumerate() {
                        let selected = cur_type == i as i32;
                        if ui.selectable_label(selected, *name).clicked() {
                            state.shared.params[P_TYPE].store(i as f32, Ordering::Relaxed);
                            state.shared.dirty_params[P_TYPE].store(true, Ordering::Relaxed);
                        }
                    }
                    // Live cutoff readout in Hz/kHz.
                    let hz = cutoff_units_to_hz(state.shared.params[P_CUTOFF].load(Ordering::Relaxed));
                    let label = if hz < 1000.0 {
                        format!("@ {:.0} Hz", hz)
                    } else {
                        format!("@ {:.2} kHz", hz / 1000.0)
                    };
                    ui.label(egui::RichText::new(label).color(core_gui::GREEN_DIM).monospace().small());
                });
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_CUTOFF], &PARAMS[P_CUTOFF], &state.shared.dirty_params[P_CUTOFF], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_CUTOFF);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_RESO], &PARAMS[P_RESO], &state.shared.dirty_params[P_RESO], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_RESO);
            });

            core_gui::section(ui, "Drive", |ui| {
                // DriveType as a labelled selectable row.
                let cur_drv = state.shared.params[P_DRV_TYPE].load(Ordering::Relaxed).round() as i32;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Mode").color(core_gui::GREEN).monospace());
                    for (i, name) in DRV_NAMES.iter().enumerate() {
                        let selected = cur_drv == i as i32;
                        if ui.selectable_label(selected, *name).clicked() {
                            state.shared.params[P_DRV_TYPE].store(i as f32, Ordering::Relaxed);
                            state.shared.dirty_params[P_DRV_TYPE].store(true, Ordering::Relaxed);
                        }
                    }
                });
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_DRIVE], &PARAMS[P_DRIVE], &state.shared.dirty_params[P_DRIVE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_DRIVE);
            });

            core_gui::section(ui, "LFO", |ui| {
                let cur_shp = state.shared.params[P_LFO_SHP].load(Ordering::Relaxed).round() as i32;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Shape").color(core_gui::GREEN).monospace());
                    for (i, name) in SHAPE_NAMES.iter().enumerate() {
                        let selected = cur_shp == i as i32;
                        if ui.selectable_label(selected, *name).clicked() {
                            state.shared.params[P_LFO_SHP].store(i as f32, Ordering::Relaxed);
                            state.shared.dirty_params[P_LFO_SHP].store(true, Ordering::Relaxed);
                        }
                    }
                });
                // Sync toggle + Div label
                let sync_on = state.shared.params[P_LFO_SYNC].load(Ordering::Relaxed) >= 0.5;
                ui.horizontal(|ui| {
                    if ui.selectable_label(sync_on, "Sync (host BPM)").clicked() {
                        let v = if sync_on { 0.0 } else { 1.0 };
                        state.shared.params[P_LFO_SYNC].store(v, Ordering::Relaxed);
                        state.shared.dirty_params[P_LFO_SYNC].store(true, Ordering::Relaxed);
                    }
                    if sync_on {
                        let div = state.shared.params[P_LFO_DIV].load(Ordering::Relaxed).round() as u32;
                        ui.label(egui::RichText::new(format!("Div: {}", sync_division_label(div)))
                            .color(core_gui::GREEN_BRIGHT).monospace());
                    }
                });
                if sync_on {
                    core_gui::dirty_param_row_g(ui, &state.shared.params[P_LFO_DIV], &PARAMS[P_LFO_DIV], &state.shared.dirty_params[P_LFO_DIV], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LFO_DIV);
                } else {
                    core_gui::dirty_param_row_g(ui, &state.shared.params[P_LFO_RATE], &PARAMS[P_LFO_RATE], &state.shared.dirty_params[P_LFO_RATE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LFO_RATE);
                }
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LFO_DPT], &PARAMS[P_LFO_DPT], &state.shared.dirty_params[P_LFO_DPT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LFO_DPT);
            });

            core_gui::section(ui, "Env Follow", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ENV_DPT], &PARAMS[P_ENV_DPT], &state.shared.dirty_params[P_ENV_DPT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ENV_DPT);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ENV_ATK], &PARAMS[P_ENV_ATK], &state.shared.dirty_params[P_ENV_ATK], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ENV_ATK);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ENV_REL], &PARAMS[P_ENV_REL], &state.shared.dirty_params[P_ENV_REL], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ENV_REL);
            });

            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_OUTPUT);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MIX], &PARAMS[P_MIX], &state.shared.dirty_params[P_MIX], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MIX);
            });
        });
    });
}
