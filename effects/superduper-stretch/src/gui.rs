//! egui_baseview GUI for SuperDuper Stretch.
//!
//! The visual is the capture ring with two heads on it: the write head sweeping
//! at normal speed and the read head crawling at `1/Stretch` of it. Seeing the
//! gap between them is the fastest way to understand what the plugin is doing —
//! and when you Freeze, both stop moving relative to the frozen region.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;
use superduper_synth_core::paulstretch::{BUFFER_SECONDS, WINDOW_SIZES};

use crate::presets::PRESETS;
use crate::{
    P_FREEZE, P_LENGTH, P_MIX, P_OUTPUT, P_PITCH, P_SMOOTH, P_STRETCH, P_TONAL, P_WINDOW, PARAMS,
    SharedParams,
};

pub const DEFAULT_WIDTH: u32 = 500;
pub const DEFAULT_HEIGHT: u32 = 560;
pub const MIN_WIDTH: u32 = 380;
pub const MIN_HEIGHT: u32 = 420;
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
        title: "SuperDuper Stretch".to_string(),
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
        if let Some(i) = core_gui::top_bar(
            ui,
            "SuperDuper Stretch",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "stretch_preset_combo",
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
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 62.0), egui::Sense::hover());
        draw_ring_strip(ui, state, strip);

        let gesture = || core_gui::GestureBridge {
            begin: &state.shared.gesture_begin,
            end: &state.shared.gesture_end,
        };

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Stretch", |ui| {
                param(ui, state, P_STRETCH);
                param(ui, state, P_WINDOW);
                param(ui, state, P_TONAL);
            });

            core_gui::section(ui, "Capture", |ui| {
                core_gui::dirty_toggle_row_g(
                    ui,
                    &state.shared.params[P_FREEZE],
                    &PARAMS[P_FREEZE],
                    &state.shared.dirty_params[P_FREEZE],
                    gesture(),
                    P_FREEZE,
                );
                param(ui, state, P_LENGTH);
            });

            core_gui::section(ui, "Colour", |ui| {
                param(ui, state, P_SMOOTH);
                param(ui, state, P_PITCH);
            });

            core_gui::section(ui, "Output", |ui| {
                param(ui, state, P_MIX);
                param(ui, state, P_OUTPUT);
            });

            core_gui::help_block(
                ui,
                "stretch_help",
                &[
                    (
                        "What this does",
                        "It stretches time by throwing phase away. A long window is analysed, its \
                         magnitude spectrum is kept, the phases are replaced with noise, and the \
                         frames are overlap-added at a bigger hop than they were read with. \
                         Because the phases are random the frames can't comb or cancel, which is \
                         why a 20× stretch sounds like weather instead of a broken flanger.",
                    ),
                    (
                        "Stretch / Window / Tonal",
                        "Stretch is the ratio. Window is how much time each frame sees: short \
                         (85 ms) keeps rhythm and identity, long (1.4 s) is pure wash — this is \
                         the main 'how smeared' control. Tonal blends the random phase back \
                         toward the analysed phase: at 0 you get the classic smear, at 1 a plain \
                         slow-motion that still sounds like the source event. Around 0.2 is the \
                         sweet spot for turning a sung note into a pad that keeps its pitch.",
                    ),
                    (
                        "Live vs Freeze",
                        "Stretching by N× consumes input N times slower than it emits, so in Live \
                         mode the read head keeps falling behind and eventually skips forward — \
                         you hear a continuous smear of the recent past with occasional jumps. \
                         Freeze stops the recording and circles the last Length seconds forever: \
                         sing one note, hit Freeze (or a sustain pedal on CC 64), and it becomes \
                         an endless pad. Length sets how much of the take the loop wanders over.",
                    ),
                    (
                        "Smooth / Pitch",
                        "Smooth blurs the magnitude spectrum (frequency-proportional, so it \
                         doesn't erase the bass) — it removes the identity of vowels and \
                         instruments, leaving colour. Pitch shifts the spectrum itself, so ±12 \
                         gives an octave wash or a sub bed without a separate pitch shifter.",
                    ),
                    (
                        "No latency compensation",
                        "This plugin reports zero latency on purpose: a stretched signal isn't \
                         sample-aligned with its input in any meaningful way, so there is nothing \
                         for the DAW's PDC to fix. Don't expect phase coherence on a parallel \
                         bus — use it as a send/return or on its own track.",
                    ),
                    (
                        "Chain tips",
                        "Stretch → Formant is the 'voice becomes kubyz' pair: this makes the \
                         endless bed out of your voice, Formant makes it pronounce vowels. \
                         Stretch → Supermass for infinite ambient. Put Granular AFTER it for \
                         texture on top of the wash, or BEFORE it to stretch a cloud.",
                    ),
                ],
            );
        });
    });
}

/// The capture ring: write head (normal speed, parked when frozen) and the read
/// head crawling at 1/Stretch. The gap between them *is* the algorithm.
fn draw_ring_strip(ui: &mut egui::Ui, state: &GuiState, rect: egui::Rect) {
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
    let w_phase = shared.write_phase.load(Ordering::Relaxed).clamp(0.0, 1.0);
    let r_phase = shared.read_phase.load(Ordering::Relaxed).clamp(0.0, 1.0);
    let stretch = shared.params[P_STRETCH].load(Ordering::Relaxed);
    let win_idx = (shared.params[P_WINDOW].load(Ordering::Relaxed).round().max(0.0) as usize)
        .min(WINDOW_SIZES.len() - 1);
    let sr = shared.sample_rate.load(Ordering::Relaxed).max(8_000.0);
    let win_ms = WINDOW_SIZES[win_idx] as f32 * 1000.0 / sr;

    for s in 1..BUFFER_SECONDS as usize {
        let x = rect.left() + rect.width() * (s as f32 / BUFFER_SECONDS);
        painter.line_segment(
            [egui::pos2(x, rect.top() + 18.0), egui::pos2(x, rect.bottom() - 6.0)],
            egui::Stroke::new(0.5, core_gui::GREEN_FAINT),
        );
    }

    // Read head (the slow one) — thicker, brighter: it's what you're hearing.
    let rx = rect.left() + rect.width() * r_phase;
    painter.line_segment(
        [egui::pos2(rx, rect.top() + 16.0), egui::pos2(rx, rect.bottom() - 6.0)],
        egui::Stroke::new(2.5, core_gui::GREEN_BRIGHT),
    );
    // Write head.
    let wx = rect.left() + rect.width() * w_phase;
    painter.line_segment(
        [egui::pos2(wx, rect.top() + 16.0), egui::pos2(wx, rect.bottom() - 6.0)],
        egui::Stroke::new(1.5, if frozen { core_gui::GREEN_FAINT } else { core_gui::GREEN_DIM }),
    );

    painter.text(
        rect.min + egui::vec2(8.0, 4.0),
        egui::Align2::LEFT_TOP,
        if frozen {
            format!("FROZEN — looping {:.1} s", shared.params[P_LENGTH].load(Ordering::Relaxed))
        } else {
            format!("live — {BUFFER_SECONDS:.0} s ring")
        },
        egui::FontId::monospace(11.0),
        if frozen { core_gui::GREEN_BRIGHT } else { core_gui::GREEN },
    );
    painter.text(
        egui::pos2(rect.right() - 8.0, rect.top() + 4.0),
        egui::Align2::RIGHT_TOP,
        format!("{stretch:.1}×   {win_ms:.0} ms window"),
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
