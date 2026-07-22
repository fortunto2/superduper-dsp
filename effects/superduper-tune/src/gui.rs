//! egui_baseview GUI for SuperDuper Tune.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::scale;
use crate::{
    SharedParams, P_AMOUNT, P_FORMANT, P_KEY, P_MIX, P_OUTPUT, P_RETUNE, P_SCALE, P_TARGET, PARAMS,
};

const TARGET_NAMES: [&str; 3] = ["Scale", "MIDI", "Sidechain"];

pub const DEFAULT_WIDTH: u32 = 460;
pub const DEFAULT_HEIGHT: u32 = 440;
pub const MIN_WIDTH: u32 = 360;
pub const MIN_HEIGHT: u32 = 340;
pub const MAX_WIDTH: u32 = 1200;
pub const MAX_HEIGHT: u32 = 1000;

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
        title: "SuperDuper Tune".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let initial_preset_idx = (shared.active_preset.load(std::sync::atomic::Ordering::Relaxed)
        as usize)
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
    let scale_names: Vec<&str> = scale::SCALES.iter().map(|s| s.0).collect();
    egui::CentralPanel::default().show(ctx, |ui| {
        if let Some(i) = core_gui::top_bar(
            ui,
            "SuperDuper Tune",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "tune_preset_combo",
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

        // ---- Auto-Tune-style pitch wheel + keyboard ------------------------
        {
            use std::sync::atomic::Ordering::Relaxed;
            let hz = state.shared.detected_hz.load(Relaxed);
            let corr = state.shared.correction_st.load(Relaxed);
            let key = state.shared.params[P_KEY].load(Relaxed).round().clamp(0.0, 11.0) as u8;
            let sidx = (state.shared.params[P_SCALE].load(Relaxed).round() as usize)
                .min(scale::NUM_SCALES - 1);
            let mask = scale::SCALES[sidx].1;

            let (wheel_rect, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 188.0), egui::Sense::hover());
            crate::wheel::draw_wheel(ui.painter(), wheel_rect, hz, corr);

            ui.add_space(4.0);
            let (kb_rect, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 64.0), egui::Sense::hover());
            crate::wheel::draw_keyboard(ui.painter(), kb_rect, hz, corr, key, mask);
        }

        ui.add_space(6.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Target", |ui| {
                choice(ui, state, P_TARGET, &TARGET_NAMES);
                choice(ui, state, P_KEY, &scale::KEY_NAMES);
                choice(ui, state, P_SCALE, &scale_names);
            });
            core_gui::section(ui, "Tune", |ui| {
                param(ui, state, P_RETUNE);
                param(ui, state, P_AMOUNT);
                param(ui, state, P_FORMANT);
            });
            core_gui::section(ui, "Output", |ui| {
                param(ui, state, P_MIX);
                param(ui, state, P_OUTPUT);
            });

            core_gui::help_block(
                ui,
                "tune_help",
                &[
                    (
                        "Target — where the pitch snaps to",
                        "Scale = snap to the nearest note in Key + Scale (classic autotune). \
                         MIDI = pull the voice to a note you play on a MIDI keyboard routed to \
                         this plugin (graph mode). Sidechain = follow the pitch of a reference \
                         audio input (route a synth/vocal into the 'Reference' input) — sing to \
                         a melody.",
                    ),
                    (
                        "Retune Speed = the effect",
                        "0 ms = instant hard tune, the T-Pain / robot snap. Raise it (80–200 ms) \
                         and the correction glides — natural, transparent pitch fixing that keeps \
                         vibrato and slides. Amount blends the correction in (100% = full snap, \
                         50% = halfway).",
                    ),
                    (
                        "Formant stays independent",
                        "Correction shifts pitch by moving PSOLA grain spacing, so the timbre \
                         rides along — no chipmunking. Formant then shifts the throat/body size \
                         separately for character (raise it for a bright doll, drop it for a \
                         darker voice) without changing the tuned note.",
                    ),
                    (
                        "Mono voice in",
                        "Tuned for a single voice / solo line — it tracks one pitch at a time. \
                         Clean, monophonic input tunes best. It reports its PSOLA latency to the \
                         host for delay compensation.",
                    ),
                ],
            );
        });
    });
}

fn param(ui: &mut egui::Ui, state: &GuiState, idx: usize) {
    core_gui::dirty_param_row_g(
        ui,
        &state.shared.params[idx],
        &PARAMS[idx],
        &state.shared.dirty_params[idx],
        core_gui::GestureBridge {
            begin: &state.shared.gesture_begin,
            end: &state.shared.gesture_end,
        },
        idx,
    );
}

fn choice(ui: &mut egui::Ui, state: &GuiState, idx: usize, options: &[&str]) {
    core_gui::dirty_choice_row_g(
        ui,
        &state.shared.params[idx],
        &PARAMS[idx],
        options,
        &state.shared.dirty_params[idx],
        core_gui::GestureBridge {
            begin: &state.shared.gesture_begin,
            end: &state.shared.gesture_end,
        },
        idx,
    );
}

fn apply_preset(state: &mut GuiState, index: usize) {
    if let Some(preset) = PRESETS.get(index) {
        crate::presets::apply(&state.shared, preset);
        state
            .shared
            .active_preset
            .store(index as u32, std::sync::atomic::Ordering::Relaxed);
    }
}
