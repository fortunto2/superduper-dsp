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
    let initial_preset_idx = (shared.active_preset.load(std::sync::atomic::Ordering::Relaxed) as usize)
        .min(PRESETS.len().saturating_sub(1));
    let state = GuiState {
        shared, resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(initial_preset_idx),
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
            state.shared.active_preset.store(i as u32, std::sync::atomic::Ordering::Relaxed);
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
        ui.add_space(2.0);
        draw_mini_keyboard(ui, state);
        ui.label(egui::RichText::new("MIDI map (any octave): C·Kick · D·Snare · E·HHc · F·HHo · G·Clap · A·Cowbell    (C#/D#/F#/G#/A#/B pass through to bass synth)")
            .color(core_gui::GREEN_DIM).monospace().small());
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

/// A clickable one-octave keyboard with voice labels on the white
/// keys that trigger drums (C/D/E/F/G/A). The two non-drum keys
/// (B + all 5 black keys) are dimmed and labelled "pass" — clicking
/// them does nothing here (in a DAW they'd go out the note-output
/// port to a chained synth).
fn draw_mini_keyboard(ui: &mut egui::Ui, state: &GuiState) {
    let total_w = ui.available_width();
    let h = 56.0;
    let (rect, _resp) = ui.allocate_exact_size(
        egui::vec2(total_w, h),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, core_gui::PANEL_BG);

    // 7 white keys side by side.
    let white_count = 7.0;
    let white_w = rect.width() / white_count;
    // Drum voice that each white-key class maps to.
    let white_voice: [(&str, Option<usize>); 7] = [
        ("C·Kick",  Some(0)),
        ("D·Snare", Some(1)),
        ("E·HHc",   Some(2)),
        ("F·HHo",   Some(3)),
        ("G·Clap",  Some(4)),
        ("A·Cowb",  Some(5)),
        ("B·pass",  None),
    ];
    let inv_id = ui.next_auto_id().with("drum_keyboard");
    let _ = inv_id; // suppress unused warning; we use explicit ids below.

    for (i, (label, voice)) in white_voice.iter().enumerate() {
        let key_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left() + i as f32 * white_w, rect.top()),
            egui::vec2(white_w - 1.0, rect.height()),
        );
        let resp = ui.interact(
            key_rect,
            egui::Id::new(("drum_key_white", i)),
            egui::Sense::click(),
        );
        let is_voice = voice.is_some();
        let lit = if let Some(v) = voice {
            state.shared.voice_pulse[*v].load(Ordering::Relaxed)
        } else {
            0.0
        };
        let hover = if resp.hovered() && is_voice { 0.3 } else { 0.0 };
        let bright = (lit + hover).clamp(0.0, 1.0);
        // White keys: warm cream when armed, brighter when lit.
        let base = if is_voice { (230.0, 230.0, 200.0) } else { (140.0, 140.0, 130.0) };
        let r = (base.0 + bright * (255.0 - base.0)) as u8;
        let g = (base.1 + bright * (255.0 - base.1)) as u8;
        let b = (base.2 + bright * (255.0 - base.2)) as u8;
        painter.rect_filled(key_rect, 2.0, egui::Color32::from_rgb(r, g, b));
        painter.rect_stroke(
            key_rect, 2.0,
            egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
            egui::StrokeKind::Inside,
        );
        // Label rotated upright in the centre.
        painter.text(
            egui::pos2(key_rect.center().x, key_rect.bottom() - 10.0),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::monospace(10.0),
            if is_voice { egui::Color32::from_rgb(40, 40, 40) }
            else { egui::Color32::from_rgb(90, 90, 90) },
        );
        if resp.clicked() {
            if let Some(v) = voice {
                state.shared.voice_trigger_request[*v].store(0.9, Ordering::Release);
            }
        }
    }

    // Black keys — drawn ON TOP of the whites at the standard
    // C#-D# (skip) F#-G#-A# (skip) layout. All pass-through.
    let black_w = white_w * 0.55;
    let black_h = rect.height() * 0.62;
    // x-offsets (in white-key positions from C): C# = 1.0 - half_black,
    // D# = 2.0 - half_black, F# = 4.0 - half_black, etc.
    let black_positions = [1.0_f32, 2.0, 4.0, 5.0, 6.0];
    let black_labels = ["C#", "D#", "F#", "G#", "A#"];
    for (i, &pos) in black_positions.iter().enumerate() {
        let cx = rect.left() + pos * white_w;
        let key_rect = egui::Rect::from_min_size(
            egui::pos2(cx - black_w * 0.5, rect.top()),
            egui::vec2(black_w, black_h),
        );
        let resp = ui.interact(
            key_rect,
            egui::Id::new(("drum_key_black", i)),
            egui::Sense::hover(),
        );
        let hover = if resp.hovered() { 0.25 } else { 0.0 };
        let r = (32.0 + hover * 40.0) as u8;
        let g = (40.0 + hover * 60.0) as u8;
        let b = (32.0 + hover * 40.0) as u8;
        painter.rect_filled(key_rect, 2.0, egui::Color32::from_rgb(r, g, b));
        painter.text(
            egui::pos2(key_rect.center().x, key_rect.bottom() - 6.0),
            egui::Align2::CENTER_CENTER,
            black_labels[i],
            egui::FontId::monospace(8.0),
            egui::Color32::from_rgb(150, 150, 150),
        );
    }
}
