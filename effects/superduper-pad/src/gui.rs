use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{
    PARAMS, P_ATTACK, P_CUTOFF, P_DECAY, P_DRIVE, P_MODULATION, P_OUTPUT, P_RELEASE, P_RESONANCE,
    P_SUSTAIN, P_WIDTH, SharedParams,
};

pub const DEFAULT_WIDTH: u32 = 560;
pub const DEFAULT_HEIGHT: u32 = 620;
pub const MIN_WIDTH: u32 = 420;
pub const MIN_HEIGHT: u32 = 460;
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
        title: "SuperDuper Pad".to_string(),
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
            "SuperDuper Pad",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "pad_preset_combo",
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

        // Live polyphony readout — saves a console dive when debugging
        // why a held chord stops sounding (voice steal can be quiet).
        let active = state.shared.active_voices.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            ui.label(format!("Voices: {active} / {}", crate::VOICE_COUNT));
        });
        ui.add_space(4.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Filter", |ui| {
                core_gui::dirty_param_row(ui, &state.shared.params[P_CUTOFF], &PARAMS[P_CUTOFF], &state.shared.dirty_params[P_CUTOFF]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_RESONANCE], &PARAMS[P_RESONANCE], &state.shared.dirty_params[P_RESONANCE]);
            });
            core_gui::section(ui, "Motion", |ui| {
                core_gui::dirty_param_row(ui, &state.shared.params[P_MODULATION], &PARAMS[P_MODULATION], &state.shared.dirty_params[P_MODULATION]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_DRIVE], &PARAMS[P_DRIVE], &state.shared.dirty_params[P_DRIVE]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_WIDTH], &PARAMS[P_WIDTH], &state.shared.dirty_params[P_WIDTH]);
            });
            core_gui::section(ui, "Envelope", |ui| {
                core_gui::dirty_param_row(ui, &state.shared.params[P_ATTACK], &PARAMS[P_ATTACK], &state.shared.dirty_params[P_ATTACK]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_DECAY], &PARAMS[P_DECAY], &state.shared.dirty_params[P_DECAY]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_SUSTAIN], &PARAMS[P_SUSTAIN], &state.shared.dirty_params[P_SUSTAIN]);
                core_gui::dirty_param_row(ui, &state.shared.params[P_RELEASE], &PARAMS[P_RELEASE], &state.shared.dirty_params[P_RELEASE]);
            });
            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT]);
            });
        });
    });
}
