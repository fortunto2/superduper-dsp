//! Looper GUI — four track strips with Rec / Play / Overdub / Clear
//! plus a master row with Sync, Bars, Dry, Output.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::track::{TrackCommand, TrackState};
use crate::{
    submit_track_command, track_fb_idx, track_level_idx, track_mute_idx,
    PARAMS, P_BARS, P_DRY, P_MASTER, P_SYNC, SharedParams, TRACK_COUNT,
};

pub const DEFAULT_WIDTH: u32 = 720;
pub const DEFAULT_HEIGHT: u32 = 500;
pub const MIN_WIDTH: u32 = 560;
pub const MIN_HEIGHT: u32 = 400;
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
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P, shared: SharedParams, resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Looper".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let state = GuiState { shared, resize, applied_size: (initial_w, initial_h) };
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
        let _ = core_gui::top_bar(
            ui, "SuperDuper Looper",
            env!("SDSP_BUILD_NUM"), env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "looper_dummy_combo", &[""][..], &mut None::<usize>,
        );
        core_gui::ab_init_bar(
            ui, &state.shared.ab_snapshot,
            &state.shared.params, PARAMS, &state.shared.dirty_params,
        );

        // Header info — BPM, sync mode.
        let bpm = state.shared.host_bpm.load(Ordering::Relaxed);
        let sync_on = state.shared.params[P_SYNC].load(Ordering::Relaxed) >= 0.5;
        let bars = state.shared.params[P_BARS].load(Ordering::Relaxed).round() as u32;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("Host BPM: {:.1}", bpm))
                .color(core_gui::GREEN_BRIGHT).monospace());
            ui.label(egui::RichText::new(if sync_on { "  ·  Sync: ON" } else { "  ·  Sync: OFF" })
                .color(if sync_on { core_gui::GREEN_BRIGHT } else { core_gui::GREEN_DIM })
                .monospace());
            ui.label(egui::RichText::new(format!("  ·  Bars: {}",
                if bars == 0 { "Auto".to_string() } else { bars.to_string() }))
                .color(core_gui::GREEN).monospace());
        });

        ui.add_space(6.0);

        // Four track strips side by side.
        let strip_w = (ui.available_width() / TRACK_COUNT as f32 - 8.0).max(140.0);
        ui.horizontal(|ui| {
            for t in 0..TRACK_COUNT {
                draw_track_strip(ui, state, t, strip_w);
                ui.add_space(4.0);
            }
        });

        ui.add_space(8.0);
        core_gui::section(ui, "Master", |ui| {
            core_gui::dirty_toggle_row_g(ui, &state.shared.params[P_SYNC], &PARAMS[P_SYNC], &state.shared.dirty_params[P_SYNC], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_SYNC);
            core_gui::dirty_param_row_g(ui, &state.shared.params[P_BARS], &PARAMS[P_BARS], &state.shared.dirty_params[P_BARS], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_BARS);
            core_gui::dirty_param_row_g(ui, &state.shared.params[P_DRY], &PARAMS[P_DRY], &state.shared.dirty_params[P_DRY], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_DRY);
            core_gui::dirty_param_row_g(ui, &state.shared.params[P_MASTER], &PARAMS[P_MASTER], &state.shared.dirty_params[P_MASTER], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MASTER);
        });

        ui.add_space(4.0);
        ui.label(egui::RichText::new(
            "MIDI CC map (any channel):  Rec = CC 20-23  ·  Play/Stop = CC 24-27  ·  Overdub = CC 28-31  ·  Clear = CC 60-63   (press = value ≥64)")
            .color(core_gui::GREEN_DIM).monospace().small());
    });
}

fn draw_track_strip(ui: &mut egui::Ui, state: &GuiState, t: usize, w: f32) {
    let state_atom = TrackState::from_u32(
        state.shared.track_state[t].load(Ordering::Relaxed)
    );
    let progress = state.shared.track_progress[t].load(Ordering::Relaxed);
    egui::Frame::group(ui.style())
        .fill(core_gui::PANEL_BG)
        .show(ui, |ui| {
            ui.set_min_width(w);

            // Title + state indicator dot.
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("Track {}", t + 1))
                    .color(core_gui::GREEN_BRIGHT).monospace().strong());
                let (label, colour) = match state_atom {
                    TrackState::Empty => ("Empty", egui::Color32::from_rgb(60, 80, 60)),
                    TrackState::Recording => ("REC", egui::Color32::from_rgb(255, 90, 90)),
                    TrackState::Playing => ("Play", egui::Color32::from_rgb(120, 220, 140)),
                    TrackState::Overdubbing => ("OVERDUB", egui::Color32::from_rgb(255, 200, 80)),
                    TrackState::Stopped => ("Stop", egui::Color32::from_rgb(140, 140, 140)),
                };
                ui.label(egui::RichText::new(label).color(colour).monospace());
            });

            // Progress bar with loop position.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(w - 8.0, 8.0), egui::Sense::hover());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 2.0, egui::Color32::from_rgb(20, 24, 22));
            let fill_w = rect.width() * progress.clamp(0.0, 1.0);
            if fill_w > 1.0 {
                let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height()));
                let colour = match state_atom {
                    TrackState::Recording => egui::Color32::from_rgb(180, 60, 60),
                    TrackState::Overdubbing => egui::Color32::from_rgb(220, 170, 60),
                    _ => egui::Color32::from_rgb(80, 160, 100),
                };
                painter.rect_filled(fill_rect, 2.0, colour);
            }

            ui.add_space(4.0);

            // Four command buttons in a 2x2 grid.
            egui::Grid::new(format!("looper_track_grid_{t}"))
                .num_columns(2).spacing([4.0, 4.0])
                .show(ui, |ui| {
                    if styled_button(ui, "Rec",
                        matches!(state_atom, TrackState::Recording),
                        egui::Color32::from_rgb(180, 60, 60),
                    ).clicked() {
                        submit_track_command(&state.shared, t, TrackCommand::Rec);
                    }
                    let playstop_label = if matches!(state_atom, TrackState::Playing | TrackState::Overdubbing)
                        { "Stop" } else { "Play" };
                    if styled_button(ui, playstop_label,
                        matches!(state_atom, TrackState::Playing | TrackState::Overdubbing),
                        egui::Color32::from_rgb(80, 160, 100),
                    ).clicked() {
                        submit_track_command(&state.shared, t, TrackCommand::PlayStop);
                    }
                    ui.end_row();
                    if styled_button(ui, "Overdub",
                        matches!(state_atom, TrackState::Overdubbing),
                        egui::Color32::from_rgb(220, 170, 60),
                    ).clicked() {
                        submit_track_command(&state.shared, t, TrackCommand::Overdub);
                    }
                    if styled_button(ui, "Clear", false,
                        egui::Color32::from_rgb(120, 80, 80),
                    ).clicked() {
                        submit_track_command(&state.shared, t, TrackCommand::Clear);
                    }
                    ui.end_row();
                });

            ui.add_space(4.0);

            // Per-track sliders.
            let level_idx = track_level_idx(t);
            let fb_idx = track_fb_idx(t);
            let mute_idx = track_mute_idx(t);
            core_gui::dirty_param_row_g(ui, &state.shared.params[level_idx], &PARAMS[level_idx], &state.shared.dirty_params[level_idx], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, level_idx);
            core_gui::dirty_param_row_g(ui, &state.shared.params[fb_idx], &PARAMS[fb_idx], &state.shared.dirty_params[fb_idx], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, fb_idx);

            // Mute toggle as a button instead of a slider for clarity.
            let muted = state.shared.params[mute_idx].load(Ordering::Relaxed) >= 0.5;
            if styled_button(ui, if muted { "[ Muted ]" } else { "[ Un-mute ]" },
                muted, egui::Color32::from_rgb(180, 80, 130),
            ).clicked() {
                state.shared.params[mute_idx]
                    .store(if muted { 0.0 } else { 1.0 }, Ordering::Relaxed);
                state.shared.dirty_params[mute_idx].store(true, Ordering::Relaxed);
            }
        });
}

/// Compact selectable-style button that lights up when `active` is
/// true. Used for the transport buttons + mute toggle.
fn styled_button(ui: &mut egui::Ui, text: &str, active: bool, bright: egui::Color32) -> egui::Response {
    let label = egui::RichText::new(text).monospace().size(12.0);
    let label = if active {
        label.color(egui::Color32::from_rgb(20, 28, 22)).strong()
    } else {
        label.color(core_gui::GREEN)
    };
    let button = if active {
        egui::Button::new(label).fill(bright)
    } else {
        egui::Button::new(label).fill(core_gui::TRACK_BG)
    };
    ui.add_sized([64.0, 26.0], button)
}
