//! egui_baseview GUI for SuperDuper Granular.
//!
//! The visual is the capture buffer: a strip with the write head sweeping across
//! it (and stopping dead when you Freeze — which is exactly what you want to
//! see), plus a live grain count so "density" stops being an abstract number.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::granular::{BUFFER_SECONDS, MAX_GRAINS};
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{
    P_DENSITY, P_DIV, P_FEEDBACK, P_FREEZE, P_JITTER, P_MIX, P_OUTPUT, P_PITCH, P_POSITION,
    P_REVERSE, P_SHAPE, P_SIZE, P_SPRAY, P_SPREAD, P_SYNC, PARAMS, SharedParams,
};

pub const DEFAULT_WIDTH: u32 = 520;
pub const DEFAULT_HEIGHT: u32 = 640;
pub const MIN_WIDTH: u32 = 400;
pub const MIN_HEIGHT: u32 = 460;
pub const MAX_WIDTH: u32 = 1400;
pub const MAX_HEIGHT: u32 = 1200;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

const SHAPE_NAMES: [&str; 3] = ["Hann", "Tukey", "Perc"];
const SYNC_NAMES: [&str; 2] = ["Free", "Sync"];

struct GuiState {
    shared: SharedParams,
    resize: ResizeBridge,
    applied_size: (u32, u32),
    selected_preset: Option<usize>,
    preset_names: Vec<&'static str>,
    /// Smoothed grain count so the readout doesn't strobe.
    grain_avg: f32,
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P,
    shared: SharedParams,
    resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Granular".to_string(),
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
        grain_avg: 0.0,
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
            "SuperDuper Granular",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "granular_preset_combo",
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

        let (strip, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 64.0), egui::Sense::hover());
        draw_buffer_strip(ui, state, strip);

        let gesture = || core_gui::GestureBridge {
            begin: &state.shared.gesture_begin,
            end: &state.shared.gesture_end,
        };

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Capture", |ui| {
                core_gui::dirty_toggle_row_g(
                    ui,
                    &state.shared.params[P_FREEZE],
                    &PARAMS[P_FREEZE],
                    &state.shared.dirty_params[P_FREEZE],
                    gesture(),
                    P_FREEZE,
                );
                param(ui, state, P_POSITION);
                param(ui, state, P_SPRAY);
                param(ui, state, P_FEEDBACK);
            });

            core_gui::section(ui, "Grains", |ui| {
                core_gui::dirty_choice_row_g(
                    ui,
                    &state.shared.params[P_SYNC],
                    &PARAMS[P_SYNC],
                    &SYNC_NAMES,
                    &state.shared.dirty_params[P_SYNC],
                    gesture(),
                    P_SYNC,
                );
                if state.shared.params[P_SYNC].load(Ordering::Relaxed) >= 0.5 {
                    param(ui, state, P_DIV);
                } else {
                    param(ui, state, P_DENSITY);
                }
                param(ui, state, P_SIZE);
                core_gui::dirty_choice_row_g(
                    ui,
                    &state.shared.params[P_SHAPE],
                    &PARAMS[P_SHAPE],
                    &SHAPE_NAMES,
                    &state.shared.dirty_params[P_SHAPE],
                    gesture(),
                    P_SHAPE,
                );
                param(ui, state, P_REVERSE);
            });

            core_gui::section(ui, "Pitch & Space", |ui| {
                param(ui, state, P_PITCH);
                param(ui, state, P_JITTER);
                param(ui, state, P_SPREAD);
            });

            core_gui::section(ui, "Output", |ui| {
                param(ui, state, P_MIX);
                param(ui, state, P_OUTPUT);
            });

            core_gui::help_block(
                ui,
                "granular_help",
                &[
                    (
                        "What this does",
                        "The input is continuously recorded into a 6-second buffer. A scheduler \
                         spawns short windowed fragments — grains — that read from somewhere \
                         behind the write head, each with its own pitch, pan and direction. \
                         Hundreds of them per second add up to a texture instead of a sound.",
                    ),
                    (
                        "Freeze is the whole plugin",
                        "Freeze stops the recording; the cloud then chews the last few seconds \
                         forever. Sing one note, hit Freeze, and you have an endless pad made of \
                         your own voice — no sampler, no loop points. Map it to a sustain pedal \
                         (CC 64) and you can catch a moment mid-phrase with your foot.",
                    ),
                    (
                        "Density / Size / Spray / Position",
                        "Density is grains per second, Size their length. Density × Size is the \
                         overlap: below 1 you get gaps and rhythm, above ~4 a continuous wash \
                         (output is level-compensated by √overlap so turning Density up doesn't \
                         just get louder). Position sets how far behind the write head grains \
                         start — small = tight and stuttery, large = a long echo of the past. \
                         Spray randomises that per grain: 0 = all grains from the same place \
                         (coherent, pitched), 1 = scattered across the whole buffer (a smear).",
                    ),
                    (
                        "Sync",
                        "Sync replaces Density with a host-grid division, so grains fire on the \
                         beat instead of at a free rate. With small Spray and Position that turns \
                         the cloud into a beat-repeat / stutter locked to your project tempo.",
                    ),
                    (
                        "Feedback + Shape",
                        "Feedback writes the cloud's own output back into the buffer, so grains \
                         granulate grains — the source dissolves into texture over a few seconds. \
                         A DC blocker sits in that path so the loop can't build up an offset. \
                         Shape is the grain window: Hann = smooth cloud (never clicks), Tukey = \
                         flat middle so each grain keeps its own attack (sampler-ish), Perc = \
                         instant attack + exponential decay (pointillist, percussive).",
                    ),
                    (
                        "Chain tips",
                        "Granular into SuperDuper Supermass or Reverb is the classic ambient move. \
                         For the 'voice becomes instrument' trick, put SuperDuper Formant AFTER \
                         this one: the cloud gives the drone, the formants make it speak. Feed a \
                         kubyz through it with Pitch −12 for a sub layer.",
                    ),
                ],
            );
        });
    });
}

/// The capture buffer strip: a sweeping write head (frozen = stops), a bar for
/// the live grain count, and the Freeze state written out in words.
fn draw_buffer_strip(ui: &mut egui::Ui, state: &mut GuiState, rect: egui::Rect) {
    let shared = &state.shared;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, core_gui::PANEL_BG);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
        egui::epaint::StrokeKind::Outside,
    );

    let frozen = shared.params[P_FREEZE].load(Ordering::Relaxed) >= 0.5;
    let phase = shared.write_phase.load(Ordering::Relaxed).clamp(0.0, 1.0);
    let grains = shared.live_grains.load(Ordering::Relaxed) as f32;
    state.grain_avg = state.grain_avg * 0.85 + grains * 0.15;

    // Second markers across the buffer.
    for s in 1..BUFFER_SECONDS as usize {
        let x = rect.left() + rect.width() * (s as f32 / BUFFER_SECONDS);
        painter.line_segment(
            [egui::pos2(x, rect.top() + 18.0), egui::pos2(x, rect.bottom() - 14.0)],
            egui::Stroke::new(0.5, core_gui::GREEN_FAINT),
        );
    }

    // Grain-count bar along the bottom.
    let fill = (state.grain_avg / MAX_GRAINS as f32).clamp(0.0, 1.0);
    let bar = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 2.0, rect.bottom() - 12.0),
        egui::pos2(rect.left() + 2.0 + (rect.width() - 4.0) * fill, rect.bottom() - 4.0),
    );
    painter.rect_filled(bar, 2.0, core_gui::GREEN_DIM);

    // Write head — parked when frozen, which is the visual cue that matters.
    let x = rect.left() + rect.width() * phase;
    let head_col = if frozen { core_gui::GREEN_DIM } else { core_gui::GREEN_BRIGHT };
    painter.line_segment(
        [egui::pos2(x, rect.top() + 16.0), egui::pos2(x, rect.bottom() - 14.0)],
        egui::Stroke::new(2.0, head_col),
    );

    painter.text(
        rect.min + egui::vec2(8.0, 4.0),
        egui::Align2::LEFT_TOP,
        if frozen {
            format!("FROZEN — {BUFFER_SECONDS:.0} s buffer looping")
        } else {
            format!("recording — {BUFFER_SECONDS:.0} s buffer")
        },
        egui::FontId::monospace(11.0),
        head_col,
    );
    painter.text(
        egui::pos2(rect.right() - 8.0, rect.top() + 4.0),
        egui::Align2::RIGHT_TOP,
        format!("{:.0} grains", state.grain_avg),
        egui::FontId::monospace(11.0),
        core_gui::GREEN,
    );
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

fn apply_preset(state: &mut GuiState, index: usize) {
    // Must go through apply_preset_idx, not presets::apply: a preset's value
    // vector includes the Preset param's own slot, which `from_overrides` fills
    // with the table default (0). Writing that and then only updating
    // `active_preset` makes the next process() block read P_PRESET = 0 against
    // active = index, ask the main thread to "recall preset 0", and silently
    // revert everything the user just picked. apply_preset_idx writes the index
    // into P_PRESET as well, so recall detection stays quiet.
    crate::apply_preset_idx(&state.shared, index);
}
