//! GUI for SuperDuper Drum — six channel strips + a master row + a
//! pad-light grid that flashes on trigger.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{voice_name, voice_param_idx, P_DRIVE, P_MASTER, P_NOTE_OUT, PARAMS, SharedParams};

pub const DEFAULT_WIDTH: u32 = 760;
pub const DEFAULT_HEIGHT: u32 = 540;
pub const MIN_WIDTH: u32 = 600;
pub const MIN_HEIGHT: u32 = 420;
pub const MAX_WIDTH: u32 = 1600;
pub const MAX_HEIGHT: u32 = 1100;

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
    last_pad_decay: f32,
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P, shared: SharedParams, resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Drum".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let state = GuiState {
        shared, resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(0),
        preset_names: PRESETS.iter().map(|p| p.name).collect(),
        last_pad_decay: 0.0,
    };
    EguiWindow::open_parented(
        parent, settings, GraphicsConfig::default(), state,
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
            ui, "SuperDuper Drum",
            env!("SDSP_BUILD_NUM"), env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "drum_preset_combo", &state.preset_names, &mut state.selected_preset,
        ) {
            if let Some(preset) = PRESETS.get(i) {
                crate::presets::apply(&state.shared, preset);
            }

        core_gui::ab_init_bar(
            ui, &state.shared.ab_snapshot,
            &state.shared.params, PARAMS, &state.shared.dirty_params,
        );
        let (sdsp_scope_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 56.0),
            egui::Sense::hover(),
        );
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, sdsp_scope_rect, 48_000.0);
        }

        // Pad lights — one per voice. Decay the pulse atoms each
        // frame; voice triggers reset them to 1.0 in the audio
        // thread. Visual feedback for "what just hit".
        draw_pad_strip(ui, state);
        ui.add_space(4.0);

        // Six channel strips side by side.
        ui.horizontal(|ui| {
            for v in 0..6 {
                draw_voice_strip(ui, state, v);
                ui.add_space(4.0);
            }
        });

        ui.add_space(8.0);
        core_gui::section(ui, "Master", |ui| {
            core_gui::dirty_param_row_g(ui, &state.shared.params[P_DRIVE], &PARAMS[P_DRIVE], &state.shared.dirty_params[P_DRIVE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_DRIVE);
            core_gui::dirty_param_row_g(ui, &state.shared.params[P_MASTER], &PARAMS[P_MASTER], &state.shared.dirty_params[P_MASTER], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MASTER);
            core_gui::dirty_param_row_g(ui, &state.shared.params[P_NOTE_OUT], &PARAMS[P_NOTE_OUT], &state.shared.dirty_params[P_NOTE_OUT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_NOTE_OUT);
            ui.label(egui::RichText::new("Tip: send bass notes (outside C1-A1) to drive Wave/Kubyz on a chained synth track.")
                .color(core_gui::GREEN_DIM).monospace().small());
        });
    });
}

fn draw_pad_strip(ui: &mut egui::Ui, state: &mut GuiState) {
    let h = 36.0;
    let total_w = ui.available_width();
    let pad_w = ((total_w - 5.0 * 4.0) / 6.0).max(40.0);
    state.last_pad_decay += ui.input(|i| i.stable_dt).min(0.05);
    let decay_step = state.last_pad_decay;
    state.last_pad_decay = 0.0;

    ui.horizontal(|ui| {
        for v in 0..6 {
            let pulse = state.shared.voice_pulse[v].load(Ordering::Relaxed);
            // Decay the pulse so the light fades after a trigger.
            let new_pulse = (pulse - decay_step * 5.0).max(0.0);
            state.shared.voice_pulse[v].store(new_pulse, Ordering::Relaxed);

            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(pad_w, h), egui::Sense::click(),
            );
            // Trigger on click — write velocity into the bridge
            // atomic; audio thread consumes at the top of next block.
            if resp.clicked() {
                state.shared.voice_trigger_request[v].store(0.9, Ordering::Release);
            }
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 4.0, core_gui::PANEL_BG);
            // Pulse + hover combined for richer feedback.
            let hover_boost = if resp.hovered() { 0.25 } else { 0.0 };
            let bright = (pulse + hover_boost).clamp(0.0, 1.0);
            let r = (60.0 + bright * 150.0) as u8;
            let g = (90.0 + bright * 140.0) as u8;
            let b = (60.0 + bright * 30.0) as u8;
            painter.rect_filled(
                rect.shrink(2.0), 3.0,
                egui::Color32::from_rgb(r, g, b),
            );
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                voice_name(v),
                egui::FontId::monospace(11.0),
                core_gui::GREEN_BRIGHT,
            );
        }
    });
}

fn draw_voice_strip(ui: &mut egui::Ui, state: &GuiState, v: usize) {
    let strip_w = 110.0;
    egui::Frame::group(ui.style())
        .fill(core_gui::PANEL_BG)
        .show(ui, |ui| {
            ui.set_min_width(strip_w);
            ui.label(egui::RichText::new(voice_name(v))
                .color(core_gui::GREEN_BRIGHT).monospace().strong());
            for (offset, label) in [(0, "Tune"), (1, "Decay"), (2, "Level"), (3, "Pan")] {
                let idx = voice_param_idx(v, offset);
                let val = state.shared.params[idx].load(Ordering::Relaxed);
                let def = &PARAMS[idx];
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(label).color(core_gui::GREEN_DIM).small());
                    let mut val_local = val;
                    let slider = egui::Slider::new(
                        &mut val_local,
                        (def.min as f32)..=(def.max as f32),
                    )
                    .show_value(true)
                    .clamping(egui::SliderClamping::Always);
                    let resp = ui.add(slider);
                    if resp.drag_started() {
                        state.shared.gesture_begin[idx].store(true, Ordering::Relaxed);
                    }
                    if resp.changed() {
                        state.shared.params[idx].store(val_local, Ordering::Relaxed);
                        state.shared.dirty_params[idx].store(true, Ordering::Relaxed);
                    }
                    if resp.drag_stopped() {
                        state.shared.gesture_end[idx].store(true, Ordering::Relaxed);
                    }
                });
            }
        });
}
