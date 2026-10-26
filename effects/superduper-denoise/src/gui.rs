//! SuperDuper Denoise — mode, the knobs of the active mode, learn toggle.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{SharedParams, MODE_NAMES, PARAMS, P_AMOUNT, P_LEARN, P_MIX, P_MODE, P_OUTPUT, P_REDUCTION, P_RT60, P_SMOOTHING, P_THRESHOLD};

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
        title: "SuperDuper Denoise".to_string(),
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
        if let Some(i) = core_gui::top_bar_mix(&state.shared.scope, 
            ui,
            "SuperDuper Denoise",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "denoise_preset_combo",
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

        let g = core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end };
        let row = |ui: &mut egui::Ui, i: usize| {
            core_gui::dirty_param_row_g(ui, &state.shared.params[i], &PARAMS[i], &state.shared.dirty_params[i], g, i);
        };
        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Mode", |ui| {
                core_gui::dirty_choice_row_g(ui, &state.shared.params[P_MODE], &PARAMS[P_MODE], &MODE_NAMES, &state.shared.dirty_params[P_MODE], g, P_MODE);
            });
            let mode = state.shared.params[P_MODE].load(Ordering::Relaxed).round() as i32;
            let hint = |ui: &mut egui::Ui, t: &str| {
                ui.label(egui::RichText::new(t).color(core_gui::GREEN_DIM).monospace().small());
            };
            match mode {
                0 => core_gui::section(ui, "Spectral", |ui| {
                    row(ui, P_REDUCTION);
                    row(ui, P_THRESHOLD);
                    row(ui, P_SMOOTHING);
                    core_gui::dirty_choice_row_g(ui, &state.shared.params[P_LEARN], &PARAMS[P_LEARN], &["Off", "Learning"], &state.shared.dirty_params[P_LEARN], g, P_LEARN);
                    hint(ui, if state.shared.has_profile.load(Ordering::Relaxed) {
                        "Learned profile in use (keeps steady tones)"
                    } else {
                        "Adaptive floor. Learn on a noise-only passage to keep held notes"
                    });
                }),
                1 => core_gui::section(ui, "Dereverb", |ui| {
                    row(ui, P_AMOUNT);
                    row(ui, P_RT60);
                    hint(ui, "For a voice in a room; long sustained sounds get cut too");
                }),
                _ => core_gui::section(ui, "Neural", |ui| {
                    hint(ui, if state.shared.neural_ready.load(Ordering::Relaxed) {
                        "RNNoise: keeps the voice, removes everything else. Mix sets the strength"
                    } else {
                        "RNNoise runs at 48 kHz only: set the project to 48 kHz. Passing through now"
                    });
                }),
            }
            core_gui::section(ui, "Output", |ui| {
                row(ui, P_MIX);
                row(ui, P_OUTPUT);
            });
        });
    });
}
