//! SuperDuper Harmonic Clean GUI — pitch-locked harmonic comb denoiser.
//!
//! Top bar + A/B + preset combo, a live output-spectrum strip (so you watch
//! the harmonics stand out from the lowered noise floor), a big detected-f0
//! readout + a reduction meter, the Amount / Bandwidth / Transient controls,
//! Mix / Output, the tracker Range, and a `? help` block.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{
    SharedParams, PARAMS, P_AMOUNT, P_BANDWIDTH, P_MIX, P_MODE, P_OUTPUT, P_RANGE, P_TRANSIENT,
};

const MODE_NAMES: [&str; 2] = ["Median", "Mean"];

pub const DEFAULT_WIDTH: u32 = 560;
pub const DEFAULT_HEIGHT: u32 = 620;
pub const MIN_WIDTH: u32 = 460;
pub const MIN_HEIGHT: u32 = 500;
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
        title: "SuperDuper Harmonic".to_string(),
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

fn gesture(state: &GuiState) -> core_gui::GestureBridge<'_> {
    core_gui::GestureBridge {
        begin: &state.shared.gesture_begin,
        end: &state.shared.gesture_end,
    }
}

/// The live readout: detected fundamental + a reduction bar.
fn draw_readout(ui: &mut egui::Ui, state: &GuiState) {
    let f0 = state.shared.detected_f0.load(Ordering::Relaxed);
    let red = state.shared.reduction.load(Ordering::Relaxed).clamp(0.0, 1.0);

    let f0_label = if f0 >= 20.0 {
        format!("f0  {:.1} Hz", f0)
    } else {
        "f0  — (unvoiced)".to_string()
    };
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(f0_label)
                .color(core_gui::GREEN_BRIGHT)
                .monospace()
                .size(18.0),
        );
        ui.add_space(12.0);
        ui.label(
            egui::RichText::new("noise cut")
                .color(core_gui::GREEN_DIM)
                .monospace()
                .small(),
        );
        // Reduction bar.
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width().min(180.0), 14.0), egui::Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 3.0, egui::Color32::from_gray(30));
        let mut fill = rect;
        fill.set_width(rect.width() * red);
        painter.rect_filled(fill, 3.0, core_gui::GREEN_BRIGHT);
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{:.0}%", red * 100.0),
            egui::FontId::monospace(10.0),
            egui::Color32::from_gray(220),
        );
    });
}

fn draw(ctx: &egui::Context, state: &mut GuiState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        if let Some(i) = core_gui::top_bar(
            ui,
            "SuperDuper Harmonic",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "harmonic_preset_combo",
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

        let (scope_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 50.0),
            egui::Sense::hover(),
        );
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, scope_rect, 48_000.0);

        draw_readout(ui, state);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Denoise", |ui| {
                core_gui::dirty_choice_row_g(
                    ui,
                    &state.shared.params[P_MODE],
                    &PARAMS[P_MODE],
                    &MODE_NAMES,
                    &state.shared.dirty_params[P_MODE],
                    gesture(state),
                    P_MODE,
                );
                ui.label(
                    egui::RichText::new("Median rejects transient echo (default, best for piezo clicks); Mean = classic average")
                        .color(core_gui::GREEN_DIM)
                        .monospace()
                        .small(),
                );
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_AMOUNT], &PARAMS[P_AMOUNT], &state.shared.dirty_params[P_AMOUNT], gesture(state), P_AMOUNT);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_BANDWIDTH], &PARAMS[P_BANDWIDTH], &state.shared.dirty_params[P_BANDWIDTH], gesture(state), P_BANDWIDTH);
                ui.label(
                    egui::RichText::new("low Bandwidth = narrow keep-band = aggressive; high = gentle")
                        .color(core_gui::GREEN_DIM)
                        .monospace()
                        .small(),
                );
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_TRANSIENT], &PARAMS[P_TRANSIENT], &state.shared.dirty_params[P_TRANSIENT], gesture(state), P_TRANSIENT);
                ui.label(
                    egui::RichText::new("Transient up = plucks pass through clean (comb re-opens on attacks)")
                        .color(core_gui::GREEN_DIM)
                        .monospace()
                        .small(),
                );
            });

            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MIX], &PARAMS[P_MIX], &state.shared.dirty_params[P_MIX], gesture(state), P_MIX);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT], gesture(state), P_OUTPUT);
            });

            core_gui::section(ui, "Tracker", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_RANGE], &PARAMS[P_RANGE], &state.shared.dirty_params[P_RANGE], gesture(state), P_RANGE);
                ui.label(
                    egui::RichText::new("Range = lowest note to lock — raise it if the comb chases an octave-down ghost")
                        .color(core_gui::GREEN_DIM)
                        .monospace()
                        .small(),
                );
            });

            core_gui::help_block(
                ui,
                "harmonic_help",
                &[
                    (
                        "What it's for",
                        "Cleaning a piezo / electric kubyz (jaw-harp, khomus). A contact \
                         pickup on a metal reed grabs the musical harmonics AND a layer of \
                         inharmonic micro-rustle — finger/contact noise, reed buzz, pickup \
                         hiss — that lives between the harmonics. This keeps the harmonics \
                         and the plucks, drops the noise between them. Put it first in the \
                         chain, right after the pickup.",
                    ),
                    (
                        "Amount / Bandwidth",
                        "Amount = how hard the between-harmonic noise is cut (0 = bypass, \
                         1 = maximum). Bandwidth = how wide a strip around each harmonic is \
                         kept: narrow (low) is aggressive and squeezes more noise out but \
                         needs steady, accurate pitch; wide (high) is gentle and forgiving.",
                    ),
                    (
                        "Transient",
                        "A jaw-harp is all about the pluck. The comb averages over periods, \
                         which would smear a sudden attack — so an onset detector re-opens \
                         the comb on plucks. Transient sets how far it re-opens: turn it up \
                         if attacks sound soft or dull, down for a smoother sustained drone.",
                    ),
                    (
                        "Range",
                        "The lowest fundamental the pitch tracker will lock onto. A kubyz \
                         drone sits around 73 Hz; if the comb latches an octave down and \
                         sounds hollow, raise Range above the ghost. The f0 readout shows \
                         the current lock.",
                    ),
                ],
            );
        });
    });
}
