//! egui_baseview GUI for SuperDuper Formant.
//!
//! The hero visual is the **vowel pad**: F2 across, F1 down, the IPA vowels
//! plotted as reference dots, and a live cursor showing where the formants
//! actually are right now — which in Follow mode is the singer's mouth and in
//! Motion mode is the trajectory. You watch the vowel being sung.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;
use superduper_synth_core::kubyz::trajectory::MouthShape;

use crate::dsp::{EX_F1, EX_F2, MODE_FOLLOW, MODE_MOTION};
use crate::presets::PRESETS;
use crate::{
    write_param, P_DEPTH, P_DIV, P_DRIVE, P_F1, P_F2, P_F3, P_FOLLOW, P_GLIDE, P_MIX, P_MODE,
    P_OUTPUT, P_PATH, P_RATE, P_SHIFT, P_STEREO, P_SYNC, P_WIDTH, PARAMS, SharedParams,
};

pub const DEFAULT_WIDTH: u32 = 560;
pub const DEFAULT_HEIGHT: u32 = 720;
pub const MIN_WIDTH: u32 = 420;
pub const MIN_HEIGHT: u32 = 520;
pub const MAX_WIDTH: u32 = 1400;
pub const MAX_HEIGHT: u32 = 1200;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

const MODE_NAMES: [&str; 3] = ["Manual", "Follow", "Motion"];
const PATH_NAMES: [&str; 5] = ["Circle", "Sine", "Figure-8", "Triangle", "Line"];
const SYNC_NAMES: [&str; 2] = ["Free", "Sync"];

/// Pad axis spans — taken from the param table so the pad and the sliders can
/// never disagree.
fn f1_span() -> (f32, f32) {
    (PARAMS[P_F1].min as f32, PARAMS[P_F1].max as f32)
}
fn f2_span() -> (f32, f32) {
    (PARAMS[P_F2].min as f32, PARAMS[P_F2].max as f32)
}

/// Reference vowels for the pad. Peterson-Barney male averages + the Bashkir
/// kubyz measurement (the one that matters for this rig).
struct RefVowel {
    label: &'static str,
    f1: f32,
    f2: f32,
    f3: f32,
}

const VOWELS: &[RefVowel] = &[
    RefVowel { label: "i", f1: 270.0, f2: 2290.0, f3: 3010.0 },
    RefVowel { label: "e", f1: 530.0, f2: 1840.0, f3: 2480.0 },
    RefVowel { label: "a", f1: 730.0, f2: 1090.0, f3: 2440.0 },
    RefVowel { label: "o", f1: 570.0, f2: 840.0, f3: 2410.0 },
    RefVowel { label: "u", f1: 300.0, f2: 870.0, f3: 2240.0 },
    RefVowel { label: "kubyz", f1: 705.0, f2: 1301.0, f3: 2165.0 },
];

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
        title: "SuperDuper Formant".to_string(),
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
            "SuperDuper Formant",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "formant_preset_combo",
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

        let mode = state.shared.params[P_MODE].load(Ordering::Relaxed).round() as u32;

        // ---- Vowel pad ---------------------------------------------------
        let pad_h = 200.0f32.min(ui.available_height() * 0.45).max(120.0);
        let (pad_rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), pad_h), egui::Sense::hover());
        draw_vowel_pad(ui, state, pad_rect, mode);

        // Tracker status line — is the plugin hearing the voice, or holding?
        if mode == MODE_FOLLOW {
            let db = state.shared.track_level_db.load(Ordering::Relaxed);
            let live = state.shared.track_active.load(Ordering::Relaxed);
            let text = if live {
                format!("● listening   voice {db:.0} dBFS")
            } else {
                format!("◌ holding last vowel   voice {db:.0} dBFS")
            };
            let colour = if live { core_gui::GREEN_BRIGHT } else { core_gui::GREEN_DIM };
            ui.colored_label(colour, text);
        }

        let gesture = || core_gui::GestureBridge {
            begin: &state.shared.gesture_begin,
            end: &state.shared.gesture_end,
        };

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Articulation", |ui| {
                core_gui::dirty_choice_row_g(
                    ui,
                    &state.shared.params[P_MODE],
                    &PARAMS[P_MODE],
                    &MODE_NAMES,
                    &state.shared.dirty_params[P_MODE],
                    gesture(),
                    P_MODE,
                );
                if mode == MODE_FOLLOW {
                    param(ui, state, P_FOLLOW);
                }
                param(ui, state, P_GLIDE);
            });

            core_gui::section(ui, "Formants", |ui| {
                param(ui, state, P_F1);
                param(ui, state, P_F2);
                param(ui, state, P_F3);
                param(ui, state, P_WIDTH);
                param(ui, state, P_SHIFT);
            });

            if mode == MODE_MOTION {
                core_gui::section(ui, "Motion", |ui| {
                    core_gui::dirty_choice_row_g(
                        ui,
                        &state.shared.params[P_PATH],
                        &PARAMS[P_PATH],
                        &PATH_NAMES,
                        &state.shared.dirty_params[P_PATH],
                        gesture(),
                        P_PATH,
                    );
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
                        param(ui, state, P_RATE);
                    }
                    param(ui, state, P_DEPTH);
                    param(ui, state, P_STEREO);
                });
            }

            core_gui::section(ui, "Output", |ui| {
                param(ui, state, P_DRIVE);
                param(ui, state, P_MIX);
                param(ui, state, P_OUTPUT);
            });

            core_gui::help_block(
                ui,
                "formant_help",
                &[
                    (
                        "What this does",
                        "Three band-pass resonances — F1, F2, F3 — are what a mouth actually is. \
                         A kubyz works the same way: the reed gives a fixed drone rich in \
                         overtones, and the mouth cavity picks out which overtone you hear. This \
                         plugin puts that mouth on ANY sound. Drag the pad and the input starts \
                         pronouncing vowels.",
                    ),
                    (
                        "Voice → instrument (the point)",
                        "Set Mode = Follow, route your voice into the 'Voice' input (port 1, via \
                         the FX pin connector) and put the kubyz / drone on the insert. The \
                         tracker reads the formants out of your singing and the drone speaks \
                         them. When you stop singing the tracker GATES and the last vowel stays \
                         frozen on the drone — so a sung phrase hands over to the instrument \
                         instead of cutting out. That is the 'started singing, ended as kubyz' \
                         move: one continuous formant line, the excitation swapped underneath.",
                    ),
                    (
                        "Not the vocoder",
                        "SuperDuper Vocoder copies the whole spectral envelope of the modulator \
                         and needs the voice sounding at every instant — intelligible, robotic. \
                         This models the mechanism instead: three resonances that glide. Use the \
                         vocoder for words, this for singing and for articulating an instrument \
                         when nobody is singing at all.",
                    ),
                    (
                        "Manual / Follow / Motion",
                        "Manual: the pad is the mouth — drag it, or automate F1/F2, or drive them \
                         from gestures (CC 1 → F1, CC 74 → F2, CC 71 → Width, CC 73 → Drive, \
                         CC 76 → Depth — the live2play defaults). Follow: track the sidechain \
                         voice, Follow blends between the pad and the tracked vowel. Motion: a \
                         trajectory walks the pad on its own — Sync locks it to the host grid for \
                         a rhythmic wah, and Stereo runs L/R in anti-phase for width.",
                    ),
                    (
                        "Width, Shift, Drive",
                        "Width scales all three bandwidths: below 1 = narrow, nasal, vowel-like \
                         (and more resonant ring); above 1 = broad and airy. Shift transposes the \
                         whole formant set — down = bigger head, up = smaller. Drive saturates \
                         BEFORE the resonators, which matters: a bare sine has nothing for the \
                         formants to pick out, so add Drive when the input is too pure to \
                         articulate. Glide is the articulation speed in every mode.",
                    ),
                    (
                        "Tuning note for kubyz",
                        "The kubyz reed is a fixed pitch and it is not in equal temperament — the \
                         drone lives near C#2 / A#2+29c / D2. Match the singing to the drone, not \
                         to 12-TET, or the formant articulation will sit on a beating fundamental.",
                    ),
                ],
            );
        });
    });
}

/// The vowel pad: F2 across, F1 down (inverted so closed vowels sit at the top,
/// matching how phoneticians draw the chart). Click/drag writes F1/F2 and snaps
/// F3 from the inverse-distance-weighted reference vowels.
fn draw_vowel_pad(ui: &mut egui::Ui, state: &GuiState, rect: egui::Rect, mode: u32) {
    let shared = &state.shared;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, core_gui::PANEL_BG);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
        egui::epaint::StrokeKind::Outside,
    );

    let (f1_min, f1_max) = f1_span();
    let (f2_min, f2_max) = f2_span();
    let to_screen = |f1: f32, f2: f32| -> egui::Pos2 {
        let xf = ((f2 - f2_min) / (f2_max - f2_min)).clamp(0.0, 1.0);
        let yf = ((f1 - f1_min) / (f1_max - f1_min)).clamp(0.0, 1.0);
        egui::pos2(rect.left() + xf * rect.width(), rect.top() + yf * rect.height())
    };
    let from_screen = |p: egui::Pos2| -> (f32, f32) {
        let xf = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let yf = ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        (f1_min + yf * (f1_max - f1_min), f2_min + xf * (f2_max - f2_min))
    };

    // Reference vowel dots.
    for v in VOWELS {
        let p = to_screen(v.f1, v.f2);
        painter.circle_filled(p, 4.0, core_gui::GREEN_DIM);
        painter.text(
            egui::pos2(p.x + 7.0, p.y - 3.0),
            egui::Align2::LEFT_BOTTOM,
            v.label,
            egui::FontId::monospace(11.0),
            core_gui::GREEN,
        );
    }

    // Drag → F1/F2 (+ weighted F3). Only meaningful as a *target* in Follow
    // mode, but still worth allowing: it's the blend anchor.
    let id = ui.id().with("formant_vowel_pad");
    let response = ui.interact(rect, id, egui::Sense::click_and_drag());
    // Gestures are separate from values (lesson 21g): without Begin/End a host in
    // touch or latch mode sees a pad sweep as disconnected automation points and
    // never closes the latch — on this plugin's most-dragged control.
    if response.drag_started() {
        for idx in [P_F1, P_F2, P_F3] {
            shared.gesture_begin[idx].store(true, Ordering::Relaxed);
        }
    }
    if response.drag_stopped() {
        for idx in [P_F1, P_F2, P_F3] {
            shared.gesture_end[idx].store(true, Ordering::Relaxed);
        }
    }
    if let Some(p) = response.interact_pointer_pos() {
        if response.dragged() || response.clicked() {
            let (f1, f2) = from_screen(p);
            write_param(shared, P_F1, f1);
            write_param(shared, P_F2, f2);
            let mut acc = 0.0f32;
            let mut w_sum = 0.0f32;
            for v in VOWELS {
                let d = ((f1 - v.f1).powi(2) + (f2 - v.f2).powi(2)).sqrt().max(1.0);
                let w = 1.0 / (d * d);
                acc += w * v.f3;
                w_sum += w;
            }
            write_param(shared, P_F3, acc / w_sum.max(1e-6));
        }
    }

    // The pad target (what the params say).
    let tgt_f1 = shared.params[P_F1].load(Ordering::Relaxed);
    let tgt_f2 = shared.params[P_F2].load(Ordering::Relaxed);
    let tgt = to_screen(tgt_f1, tgt_f2);
    painter.circle_stroke(tgt, 7.0, egui::Stroke::new(1.0, core_gui::GREEN_DIM));

    // Motion trajectory preview — same excursion constants the DSP uses.
    if mode == MODE_MOTION {
        let depth = shared.params[P_DEPTH].load(Ordering::Relaxed).clamp(0.0, 1.0);
        if depth > 0.001 {
            let shape =
                MouthShape::from_index(shared.params[P_PATH].load(Ordering::Relaxed) as u32);
            let mut prev: Option<egui::Pos2> = None;
            for i in 0..=64 {
                let t = i as f32 / 64.0;
                let (x, y) = shape.point(t);
                let p = to_screen(tgt_f1 + y * EX_F1 * depth, tgt_f2 + x * EX_F2 * depth);
                if let Some(pp) = prev {
                    painter.line_segment([pp, p], egui::Stroke::new(1.0, core_gui::GREEN_FAINT));
                }
                prev = Some(p);
            }
        }
    }

    // Live cursor — where the formants ACTUALLY are this instant (tracked voice
    // in Follow, trajectory position in Motion, the target in Manual).
    let live_f1 = shared.live_f[0].load(Ordering::Relaxed);
    let live_f2 = shared.live_f[1].load(Ordering::Relaxed);
    let live = to_screen(live_f1, live_f2);
    let frozen = mode == MODE_FOLLOW && !shared.track_active.load(Ordering::Relaxed);
    let cursor_col = if frozen { core_gui::GREEN_DIM } else { core_gui::GREEN_BRIGHT };
    painter.circle_filled(live, 6.0, cursor_col);
    painter.circle_stroke(live, 9.0, egui::Stroke::new(1.0, core_gui::GREEN));

    // Axis labels + a readout of the live formants.
    painter.text(
        egui::pos2(rect.left() + 4.0, rect.top() + 3.0),
        egui::Align2::LEFT_TOP,
        "← back    F2    front →",
        egui::FontId::monospace(9.0),
        core_gui::GREEN_DIM,
    );
    painter.text(
        egui::pos2(rect.left() + 4.0, rect.bottom() - 3.0),
        egui::Align2::LEFT_BOTTOM,
        "↑ closed   F1   open ↓",
        egui::FontId::monospace(9.0),
        core_gui::GREEN_DIM,
    );
    painter.text(
        egui::pos2(rect.right() - 5.0, rect.bottom() - 3.0),
        egui::Align2::RIGHT_BOTTOM,
        format!(
            "F1 {:.0}  F2 {:.0}  F3 {:.0} Hz",
            live_f1,
            live_f2,
            shared.live_f[2].load(Ordering::Relaxed)
        ),
        egui::FontId::monospace(10.0),
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
