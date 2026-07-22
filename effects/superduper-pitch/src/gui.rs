//! egui_baseview GUI for SuperDuper Pitch.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use superduper_synth_core::gui as core_gui;

use crate::keydetect::{key_name, KEY_NONE};
use crate::presets::PRESETS;
use crate::{P_FORMANT, P_MIX, P_MODE, P_OUTPUT, P_PITCH, P_TARGET_KEY, PARAMS, SharedParams};

const MODE_NAMES: [&str; 2] = ["Voice", "Track"];

/// Target-key selector label: 0 = None, 1..24 = C major..B minor.
fn target_name(v: usize) -> &'static str {
    if v == 0 {
        "None"
    } else {
        key_name(v - 1)
    }
}

pub const DEFAULT_WIDTH: u32 = 460;
pub const DEFAULT_HEIGHT: u32 = 420;
pub const MIN_WIDTH: u32 = 360;
pub const MIN_HEIGHT: u32 = 320;
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
        title: "SuperDuper Pitch".to_string(),
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
    egui::CentralPanel::default().show(ctx, |ui| {
        if let Some(i) = core_gui::top_bar(
            ui,
            "SuperDuper Pitch",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "pitch_preset_combo",
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
        let (scope_rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 60.0), egui::Sense::hover());
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, scope_rect, 48_000.0);

        // Live detected-key readout.
        {
            use std::sync::atomic::Ordering::Relaxed;
            let ki = state.shared.key_index.load(Relaxed) as usize;
            let conf = state.shared.key_conf.load(Relaxed);
            let text = if ki >= KEY_NONE {
                "key in:  —".to_string()
            } else {
                format!("key in:  {}  ({:.0}%)", key_name(ki), conf * 100.0)
            };
            ui.label(egui::RichText::new(text).color(core_gui::GREEN_BRIGHT).monospace());
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Engine", |ui| {
                core_gui::dirty_choice_row_g(
                    ui,
                    &state.shared.params[P_MODE],
                    &PARAMS[P_MODE],
                    &MODE_NAMES,
                    &state.shared.dirty_params[P_MODE],
                    core_gui::GestureBridge {
                        begin: &state.shared.gesture_begin,
                        end: &state.shared.gesture_end,
                    },
                    P_MODE,
                );
            });
            core_gui::section(ui, "Shift", |ui| {
                param(ui, state, P_PITCH);
                param(ui, state, P_FORMANT);
            });
            core_gui::section(ui, "Output", |ui| {
                param(ui, state, P_MIX);
                param(ui, state, P_OUTPUT);
            });

            core_gui::section(ui, "Key Match (Track)", |ui| {
                key_match_ui(ui, state);
            });

            core_gui::help_block(
                ui,
                "pitch_help",
                &[
                    (
                        "Voice vs Track mode",
                        "Voice = TD-PSOLA, best quality on a solo monophonic voice, with fully \
                         independent Formant (Masyanya / bass / gender-flip). Track = phase \
                         vocoder — transposes POLYPHONIC material: whole mixes, chords, drums, a \
                         full song. Use Track to change the key of a track (Key +2 etc.); use \
                         Voice for a single voice.",
                    ),
                    (
                        "Pitch vs Formant",
                        "Pitch shifts the note up or down in semitones. Formant shifts the \
                         timbre (the 'size' of the throat/head) SEPARATELY. That split is the \
                         whole point — a manual auto-tune: raise Pitch with Formant at 0 and a \
                         voice goes higher while still sounding like the same person, instead of \
                         a chipmunk.",
                    ),
                    (
                        "Recipes",
                        "Masyanya / chipmunk = Pitch up + Formant up (small bright head). Bass / \
                         giant = Pitch down. Demon = Pitch down + Formant down (−5). Gender flip \
                         = Pitch 0, Formant ±5 (change the body, keep the melody). Keep Mix at \
                         100% for a full transform; blend it back to double the natural voice.",
                    ),
                    (
                        "Voice in, voice out",
                        "This is tuned for monophonic voice (or a solo instrument). It tracks the \
                         pitch of what you feed it, so clean, single-note input works best. It \
                         adds a few periods of latency (reported to the host for delay \
                         compensation) — mix through it freely; for the very lowest-latency \
                         live monitoring keep the shift modest.",
                    ),
                ],
            );
        });
    });
}

/// Target-key selector + "Match" button. Reads the detected key of THIS track
/// and, on Match, sets Pitch to the semitone interval (nearest octave) that
/// moves it to the chosen target key.
fn key_match_ui(ui: &mut egui::Ui, state: &GuiState) {
    use std::sync::atomic::Ordering::Relaxed;
    let shared = &state.shared;

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("target:").color(core_gui::GREEN).monospace());
        let cur = shared.params[P_TARGET_KEY].load(Relaxed).round() as usize;
        let mut sel = cur.min(24);
        egui::ComboBox::from_id_salt("pitch_target_key")
            .selected_text(target_name(sel))
            .show_ui(ui, |ui| {
                for v in 0..=24usize {
                    ui.selectable_value(&mut sel, v, target_name(v));
                }
            });
        if sel != cur {
            shared.params[P_TARGET_KEY].store(sel as f32, Relaxed);
            shared.dirty_params[P_TARGET_KEY].store(true, Relaxed);
        }

        // Compute the suggested shift (detected → target, nearest octave).
        let detected = shared.key_index.load(Relaxed) as usize;
        let suggestion = if sel > 0 {
            crate::keydetect::match_interval(detected, sel - 1)
        } else {
            None
        };

        let enabled = suggestion.is_some();
        if ui
            .add_enabled(enabled, egui::Button::new(egui::RichText::new("Match").monospace()))
            .clicked()
        {
            if let Some(iv) = suggestion {
                let pitch = (iv as f32).clamp(-24.0, 24.0);
                shared.params[P_PITCH].store(pitch, Relaxed);
                shared.dirty_params[P_PITCH].store(true, Relaxed);
                if let Some(b) = shared.gesture_begin.get(P_PITCH) {
                    b.store(true, Relaxed);
                }
                if let Some(e) = shared.gesture_end.get(P_PITCH) {
                    e.store(true, Relaxed);
                }
            }
        }
        if let Some(iv) = suggestion {
            ui.label(
                egui::RichText::new(format!("→ {iv:+} st"))
                    .color(core_gui::GREEN_DIM)
                    .monospace(),
            );
        }
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

fn apply_preset(state: &mut GuiState, index: usize) {
    if let Some(preset) = PRESETS.get(index) {
        crate::presets::apply(&state.shared, preset);
        state
            .shared
            .active_preset
            .store(index as u32, std::sync::atomic::Ordering::Relaxed);
    }
}
