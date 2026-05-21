use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::{presets, N_HARMONICS};
use crate::{
    apply_preset, write_param, PARAMS, P_ATTACK, P_BRIGHT, P_DECAY, P_F1, P_F2, P_F3,
    P_MOUTH_DEPTH, P_MOUTH_DIV, P_MOUTH_RATE, P_MOUTH_SHAPE, P_MOUTH_STEREO, P_MOUTH_SYNC,
    P_OUTPUT, P_RELEASE, P_SUSTAIN, P_TONGUE_ST, P_VEL_SHIFT, P_VOX_MIX, SharedParams,
};
use crate::trajectory::MouthShape;

/// Wraps `core_gui::param_row` so any change the user makes also raises
/// the param's dirty bit — the audio thread then emits a ParamValueEvent
/// to the host so REAPER's automation lane captures the move.
fn dirty_param_row(ui: &mut egui::Ui, shared: &SharedParams, idx: usize) {
    core_gui::learn_param_row_g(
        ui,
        &shared.params[idx],
        &PARAMS[idx],
        &shared.dirty_params[idx],
        &shared.midi_learn,
        idx,
        core_gui::GestureBridge {
            begin: &shared.gesture_begin,
            end: &shared.gesture_end,
        },
    );
}

// ---------------------------------------------------------------------------
// IPA vowel reference points (Peterson-Barney 1952, male average).
//
// `f1` ≈ jaw aperture (open/close), `f2` ≈ tongue front/back position,
// `f3` ≈ rounding/colour.  When the user drops the cursor near one of
// these we snap F3 to its reference so the vowel reads clearly without
// a separate slider.
// ---------------------------------------------------------------------------

struct Vowel {
    label: &'static str,
    f1: f32,
    f2: f32,
    f3: f32,
}

const VOWELS: &[Vowel] = &[
    Vowel { label: "i (ee)", f1: 270.0, f2: 2290.0, f3: 3010.0 },
    Vowel { label: "e (eh)", f1: 530.0, f2: 1840.0, f3: 2480.0 },
    Vowel { label: "a (ah)", f1: 730.0, f2: 1090.0, f3: 2440.0 },
    Vowel { label: "o (aw)", f1: 570.0, f2:  840.0, f3: 2410.0 },
    Vowel { label: "u (oo)", f1: 300.0, f2:  870.0, f3: 2240.0 },
];

// X axis spans F2; Y axis spans F1 (inverted — closed vowels at top).
const F2_MIN: f32 = 600.0;
const F2_MAX: f32 = 2700.0;
const F1_MIN: f32 = 200.0;
const F1_MAX: f32 = 850.0;

pub const DEFAULT_WIDTH: u32 = 760;
pub const DEFAULT_HEIGHT: u32 = 700;
pub const MIN_WIDTH: u32 = 540;
pub const MIN_HEIGHT: u32 = 520;
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
    user_preset_name: String,
    /// Domain-typed names from the typed PresetRepo. Replaces the old
    /// raw `Vec<PathBuf>` so click handlers don't have to re-parse.
    user_presets: Vec<superduper_synth_core::user_preset::PresetName>,
    selected_user_preset: Option<usize>,
    /// Transient "Saved!" / "Load failed: …" toast under the toolbar.
    last_io_msg: Option<(String, std::time::Instant)>,
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P,
    shared: SharedParams,
    resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Kubyz".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let preset_names: Vec<&'static str> = presets().iter().map(|p| p.name).collect();
    let user_presets = crate::user_extra::repo().list();
    let initial_preset_idx = (shared.active_preset.load(std::sync::atomic::Ordering::Relaxed)
        as usize)
        .min(preset_names.len().saturating_sub(1));
    let state = GuiState {
        shared,
        resize,
        applied_size: (initial_w, initial_h),
        // Restored from the persisted active_preset (defaults to 1 = Bashkir).
        selected_preset: Some(initial_preset_idx),
        preset_names,
        user_preset_name: String::new(),
        user_presets,
        selected_user_preset: None,
        last_io_msg: None,
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

/// Draw the 16 harmonic bars inside `rect`. Vertical bars centred on the
/// rect's middle line; drag a column up = louder, down = quieter (0..1 amp).
fn draw_harmonic_bars(ui: &mut egui::Ui, shared: &SharedParams, rect: egui::Rect) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, core_gui::PANEL_BG);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
        egui::epaint::StrokeKind::Outside,
    );
    let bar_w = rect.width() / N_HARMONICS as f32;
    let max_amp = 1.5_f32;
    let id = ui.id().with("kubyz_bars");
    let response = ui.interact(rect, id, egui::Sense::click_and_drag());

    // Mouse → bar index + new amplitude.
    if let Some(p) = response.interact_pointer_pos() {
        if response.dragged() || response.clicked() {
            let local_x = (p.x - rect.left()).max(0.0);
            let idx = (local_x / bar_w) as usize;
            if idx < N_HARMONICS {
                // y: bottom = 0, top = max_amp.
                let y_frac = ((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0);
                let amp = y_frac * max_amp;
                shared.harmonics[idx].store(amp, Ordering::Relaxed);
            }
        }
    }

    // Draw each bar.
    for i in 0..N_HARMONICS {
        let amp = shared.harmonics[i].load(Ordering::Relaxed).clamp(0.0, max_amp);
        let x0 = rect.left() + i as f32 * bar_w + 2.0;
        let x1 = rect.left() + (i + 1) as f32 * bar_w - 2.0;
        let bar_h = (amp / max_amp) * rect.height();
        let y0 = rect.bottom() - bar_h;
        let bar_rect = egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, rect.bottom()));
        painter.rect_filled(bar_rect, 2.0, core_gui::GREEN);
        // Tiny harmonic-number label at the top of each bar.
        painter.text(
            egui::pos2((x0 + x1) * 0.5, rect.top() + 4.0),
            egui::Align2::CENTER_TOP,
            format!("{}", i + 1),
            egui::FontId::monospace(9.0),
            core_gui::GREEN_DIM,
        );
    }
}

/// Draw the IPA vowel pad inside `rect`. Reads + writes F1/F2/F3 atomically.
fn draw_vowel_pad(ui: &mut egui::Ui, shared: &SharedParams, rect: egui::Rect) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, core_gui::PANEL_BG);
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
        egui::epaint::StrokeKind::Outside,
    );
    // Centre cross-hair for visual grounding.
    painter.line_segment(
        [
            egui::pos2(rect.center().x, rect.top()),
            egui::pos2(rect.center().x, rect.bottom()),
        ],
        egui::Stroke::new(0.5, core_gui::GREEN_FAINT),
    );
    painter.line_segment(
        [
            egui::pos2(rect.left(), rect.center().y),
            egui::pos2(rect.right(), rect.center().y),
        ],
        egui::Stroke::new(0.5, core_gui::GREEN_FAINT),
    );

    let to_screen = |f1: f32, f2: f32| -> egui::Pos2 {
        let x_frac = ((f2 - F2_MIN) / (F2_MAX - F2_MIN)).clamp(0.0, 1.0);
        let y_frac = ((f1 - F1_MIN) / (F1_MAX - F1_MIN)).clamp(0.0, 1.0);
        // Y inverted so closed vowels (low F1) sit at the top.
        egui::pos2(rect.left() + x_frac * rect.width(), rect.top() + y_frac * rect.height())
    };
    let from_screen = |p: egui::Pos2| -> (f32, f32) {
        let xf = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let yf = ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        let f2 = F2_MIN + xf * (F2_MAX - F2_MIN);
        let f1 = F1_MIN + yf * (F1_MAX - F1_MIN);
        (f1, f2)
    };

    // Reference vowel positions — labelled green dots.
    for v in VOWELS {
        let p = to_screen(v.f1, v.f2);
        painter.circle_filled(p, 4.0, core_gui::GREEN_DIM);
        painter.text(
            egui::pos2(p.x + 8.0, p.y - 4.0),
            egui::Align2::LEFT_BOTTOM,
            v.label,
            egui::FontId::monospace(11.0),
            core_gui::GREEN,
        );
    }

    // Mouse handling — drag/click sets F1/F2 and snaps F3 from the
    // weighted-nearest reference vowel.
    let id = ui.id().with("kubyz_vowel_pad");
    let response = ui.interact(rect, id, egui::Sense::click_and_drag());
    if let Some(p) = response.interact_pointer_pos() {
        if response.dragged() || response.clicked() {
            let (f1, f2) = from_screen(p);
            write_param(shared, P_F1, f1);
            write_param(shared, P_F2, f2);
            // Inverse-distance weighted F3 — closer to vowel X = more
            // influence of vowel X's F3.
            let mut acc = 0.0_f32;
            let mut w_sum = 0.0_f32;
            for v in VOWELS {
                let d = ((f1 - v.f1).powi(2) + (f2 - v.f2).powi(2)).sqrt().max(1.0);
                let w = 1.0 / (d * d);
                acc += w * v.f3;
                w_sum += w;
            }
            let f3 = acc / w_sum.max(1e-6);
            write_param(shared, P_F3, f3);
        }
    }

    // Centre dot (= the F1/F2 params). Drag-anchor for manual mode and
    // the origin of the trajectory.
    let cur_f1 = shared.params[P_F1].load(Ordering::Relaxed);
    let cur_f2 = shared.params[P_F2].load(Ordering::Relaxed);
    let cur_p = to_screen(cur_f1, cur_f2);
    painter.circle_filled(cur_p, 6.0, core_gui::GREEN_DIM);
    painter.circle_stroke(cur_p, 7.0, egui::Stroke::new(1.0, core_gui::GREEN));

    // Trajectory overlay — only visible when MouthDepth > 0. Sample the
    // shape at 64 points and draw the closed curve, then put a bright
    // dot at the current animated phase so the user *sees* the rate.
    let depth = shared.params[P_MOUTH_DEPTH]
        .load(Ordering::Relaxed)
        .clamp(0.0, 1.0);
    if depth > 0.001 {
        let shape = MouthShape::from_index(
            shared.params[P_MOUTH_SHAPE].load(Ordering::Relaxed) as u32,
        );
        // The excursion constants must match the audio thread (see lib.rs).
        let ex_f1 = 220.0_f32 * depth;
        let ex_f2 = 600.0_f32 * depth;
        let mut prev: Option<egui::Pos2> = None;
        for i in 0..=64 {
            let t = i as f32 / 64.0;
            let (tx, ty) = shape.point(t);
            let p = to_screen(cur_f1 + ty * ex_f1, cur_f2 + tx * ex_f2);
            if let Some(prev_p) = prev {
                painter.line_segment([prev_p, p], egui::Stroke::new(1.0, core_gui::GREEN));
            }
            prev = Some(p);
        }
        // Live cursor at the current phase.
        let phase = shared.mouth_phase.load(Ordering::Relaxed);
        let (tx, ty) = shape.point(phase);
        let live = to_screen(cur_f1 + ty * ex_f1, cur_f2 + tx * ex_f2);
        painter.circle_filled(live, 7.0, core_gui::GREEN_BRIGHT);
        painter.circle_stroke(live, 9.0, egui::Stroke::new(1.0, core_gui::GREEN));
    }

    // Axis labels.
    painter.text(
        egui::pos2(rect.left() + 4.0, rect.top() + 4.0),
        egui::Align2::LEFT_TOP,
        "← back   F2 →   front",
        egui::FontId::monospace(9.0),
        core_gui::GREEN_DIM,
    );
    painter.text(
        egui::pos2(rect.left() + 4.0, rect.bottom() - 4.0),
        egui::Align2::LEFT_BOTTOM,
        "↑ close   F1   open ↓",
        egui::FontId::monospace(9.0),
        core_gui::GREEN_DIM,
    );
}

/// Self-illustrating shape button: each draws a thumbnail of the
/// trajectory it picks (a tiny circle / sine / figure-8 / etc) so the
/// user picks visually without reading a label.
fn shape_icon_button(ui: &mut egui::Ui, shape: MouthShape, selected: bool) -> bool {
    let size = egui::vec2(40.0, 40.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let painter = ui.painter_at(rect);
    let bg = if selected { core_gui::GREEN_DIM } else { core_gui::PANEL_BG };
    let stroke = if selected {
        egui::Stroke::new(1.5, core_gui::GREEN_BRIGHT)
    } else if response.hovered() {
        egui::Stroke::new(1.0, core_gui::GREEN)
    } else {
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT)
    };
    painter.rect_filled(rect, 3.0, bg);
    painter.rect_stroke(rect, 3.0, stroke, egui::epaint::StrokeKind::Outside);
    // Mini trajectory preview.
    let cx = rect.center();
    let r = rect.height() * 0.32;
    let line_colour = if selected { core_gui::GREEN_BRIGHT } else { core_gui::GREEN };
    let mut prev: Option<egui::Pos2> = None;
    for i in 0..=32 {
        let t = i as f32 / 32.0;
        let (x, y) = shape.point(t);
        let p = egui::pos2(cx.x + x * r, cx.y + y * r);
        if let Some(pp) = prev {
            painter.line_segment([pp, p], egui::Stroke::new(1.4, line_colour));
        }
        prev = Some(p);
    }
    response.clicked()
}

/// Snapshot the live shared state into a typed `KubyzUserPreset`.
/// Used by Save / Save-last so both paths produce identical files.
fn snapshot_kubyz_preset(
    shared: &crate::SharedParamsInner,
    name: superduper_synth_core::user_preset::PresetName,
) -> crate::user_extra::KubyzUserPreset {
    let params: Vec<f32> = shared
        .params
        .iter()
        .map(|a| a.load(Ordering::Relaxed))
        .collect();
    let harmonics: Vec<f32> = shared
        .harmonics
        .iter()
        .map(|a| a.load(Ordering::Relaxed))
        .collect();
    superduper_synth_core::user_preset::UserPreset {
        version: superduper_synth_core::user_preset::PRESET_FORMAT_VERSION,
        name,
        params,
        extra: crate::user_extra::KubyzExtra {
            harmonics,
            formant_bw: *shared.formant_bw.lock(),
            formant_gain: *shared.formant_gain.lock(),
        },
    }
}

fn draw(ctx: &egui::Context, state: &mut GuiState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        if let Some(i) = core_gui::top_bar(
            ui,
            "SuperDuper Kubyz",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "kubyz_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            let p = presets();
            if let Some(preset) = p.get(i) {
                apply_preset(&state.shared, preset);
                state.shared.active_preset.store(i as u32, std::sync::atomic::Ordering::Relaxed);
            }
        }

        // User-preset toolbar (file-backed under
        // ~/.superduper-dsp/kubyz/presets/).
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("User:").color(core_gui::GREEN_DIM).monospace().small());
            let cur_label = state
                .selected_user_preset
                .and_then(|i| state.user_presets.get(i))
                .map(|n| n.as_str())
                .unwrap_or("—");
            let mut click: Option<(usize, superduper_synth_core::user_preset::PresetName)> = None;
            egui::ComboBox::from_id_salt("kubyz_user_preset_combo")
                .selected_text(cur_label)
                .width(160.0)
                .show_ui(ui, |ui| {
                    for (i, name) in state.user_presets.iter().enumerate() {
                        if ui
                            .selectable_label(
                                state.selected_user_preset == Some(i),
                                name.as_str(),
                            )
                            .clicked()
                        {
                            click = Some((i, name.clone()));
                        }
                    }
                });
            if let Some((i, name)) = click {
                match crate::user_extra::repo().load(&name, PARAMS.len()) {
                    Ok(preset) => {
                        for (j, v) in preset.params.iter().enumerate() {
                            if let Some(slot) = state.shared.params.get(j) {
                                slot.store(*v, Ordering::Relaxed);
                            }
                            if let Some(d) = state.shared.dirty_params.get(j) {
                                d.store(true, Ordering::Relaxed);
                            }
                        }
                        for (j, h) in preset.extra.harmonics.iter().enumerate() {
                            if let Some(slot) = state.shared.harmonics.get(j) {
                                slot.store(*h, Ordering::Relaxed);
                            }
                        }
                        *state.shared.formant_bw.lock() = preset.extra.formant_bw;
                        *state.shared.formant_gain.lock() = preset.extra.formant_gain;
                        state.selected_user_preset = Some(i);
                        state.last_io_msg =
                            Some((format!("Loaded '{name}'"), std::time::Instant::now()));
                    }
                    Err(e) => {
                        state.last_io_msg =
                            Some((format!("Load failed: {e}"), std::time::Instant::now()));
                    }
                }
            }
            if ui.small_button("↻").on_hover_text("rescan").clicked() {
                state.user_presets = crate::user_extra::repo().list();
            }
            ui.add_space(12.0);
            ui.label(egui::RichText::new("Save as:").color(core_gui::GREEN_DIM).monospace().small());
            ui.add(
                egui::TextEdit::singleline(&mut state.user_preset_name)
                    .desired_width(140.0)
                    .hint_text("name"),
            );
            if ui.button("Save").clicked() {
                match superduper_synth_core::user_preset::PresetName::new(&state.user_preset_name) {
                    Ok(name) => {
                        let preset = snapshot_kubyz_preset(&state.shared, name.clone());
                        match crate::user_extra::repo().save(&preset, PARAMS.len()) {
                            Ok(_) => {
                                state.user_preset_name.clear();
                                state.user_presets = crate::user_extra::repo().list();
                                state.selected_user_preset =
                                    state.user_presets.iter().position(|n| n == &name);
                                // Mirror as last.json too so a fresh
                                // instance picks up the same patch.
                                let _ =
                                    crate::user_extra::repo().save_last(&preset, PARAMS.len());
                                state.last_io_msg = Some((
                                    format!("Saved '{name}'"),
                                    std::time::Instant::now(),
                                ));
                            }
                            Err(e) => {
                                state.last_io_msg = Some((
                                    format!("Save failed: {e}"),
                                    std::time::Instant::now(),
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        state.last_io_msg =
                            Some((format!("Bad name: {e}"), std::time::Instant::now()));
                    }
                }
            }
        });
        if let Some((msg, ts)) = state.last_io_msg.as_ref() {
            if ts.elapsed().as_secs() < 3 {
                ui.label(
                    egui::RichText::new(msg)
                        .color(core_gui::GREEN_BRIGHT)
                        .monospace()
                        .small(),
                );
            } else {
                state.last_io_msg = None;
            }
        }

        let active = state.shared.active_voices.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            ui.label(format!("Voices: {active} / {}", crate::VOICE_COUNT));
            ui.add_space(12.0);
            ui.weak("drag bars to scribble harmonics  ·  click = set");
        });
        core_gui::ab_init_bar(
            ui,
            &state.shared.ab_snapshot,
            &state.shared.params,
            PARAMS,
            &state.shared.dirty_params,
        );
        // Live spectrum strip — log frequency, dB magnitude. More
        // informative than a raw waveform for synth work.
        let (scope_rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 80.0), egui::Sense::hover());
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, scope_rect, 48_000.0);
        ui.add_space(4.0);

        // Harmonic bars editor — top of the window, ~180 px tall.
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 180.0), egui::Sense::hover());
        draw_harmonic_bars(ui, &state.shared, rect);
        ui.add_space(8.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Vowel (formant pad)", |ui| {
                let cur_f1 = state.shared.params[P_F1].load(Ordering::Relaxed);
                let cur_f2 = state.shared.params[P_F2].load(Ordering::Relaxed);
                let cur_f3 = state.shared.params[P_F3].load(Ordering::Relaxed);
                ui.weak(format!(
                    "F1={cur_f1:4.0} Hz · F2={cur_f2:4.0} Hz · F3={cur_f3:4.0} Hz  ·  drag the dot"
                ));
                // Pad on the left + a column of 5 shape-preview buttons
                // on the right. Picking an icon writes P_MOUTH_SHAPE +
                // raises its dirty flag so REAPER records the choice.
                ui.horizontal_top(|ui| {
                    let icon_col_w = 48.0_f32;
                    let pad_w = (ui.available_width() - icon_col_w - 8.0).max(180.0);
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(pad_w, 160.0),
                        egui::Sense::hover(),
                    );
                    draw_vowel_pad(ui, &state.shared, rect);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        let active = MouthShape::from_index(
                            state.shared.params[P_MOUTH_SHAPE]
                                .load(Ordering::Relaxed) as u32,
                        );
                        for (idx, shape) in [
                            MouthShape::Circle,
                            MouthShape::Sine,
                            MouthShape::Figure8,
                            MouthShape::Triangle,
                            MouthShape::Line,
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let is_selected = shape == active;
                            if shape_icon_button(ui, shape, is_selected) {
                                write_param(
                                    &state.shared,
                                    P_MOUTH_SHAPE,
                                    idx as f32,
                                );
                            }
                            ui.add_space(2.0);
                        }
                    });
                });
            });
            core_gui::section(ui, "Formant (fine)", |ui| {
                dirty_param_row(ui, &state.shared, P_F1);
                dirty_param_row(ui, &state.shared, P_F2);
                dirty_param_row(ui, &state.shared, P_F3);
                dirty_param_row(ui, &state.shared, P_VOX_MIX);
                dirty_param_row(ui, &state.shared, P_VEL_SHIFT);
            });
            core_gui::section(ui, "Mouth Trajectory", |ui| {
                let shape_idx = state.shared.params[P_MOUTH_SHAPE]
                    .load(Ordering::Relaxed) as u32;
                ui.weak(format!(
                    "shape: {}  ·  depth=0 = manual / pad drag only  ·  ModWheel → Depth",
                    MouthShape::from_index(shape_idx).label()
                ));
                dirty_param_row(ui, &state.shared, P_MOUTH_SHAPE);
                dirty_param_row(ui, &state.shared, P_MOUTH_RATE);
                dirty_param_row(ui, &state.shared, P_MOUTH_DEPTH);
                dirty_param_row(ui, &state.shared, P_MOUTH_STEREO);
                ui.horizontal(|ui| {
                    let sync_on = state.shared.params[P_MOUTH_SYNC]
                        .load(Ordering::Relaxed)
                        >= 0.5;
                    let div_idx = state.shared.params[P_MOUTH_DIV]
                        .load(Ordering::Relaxed) as u32;
                    ui.label(if sync_on {
                        format!(
                            "sync:  on  ·  {}  ·  rate slider above is ignored",
                            superduper_synth_core::dsp_blocks::sync_division_label(div_idx)
                        )
                    } else {
                        "sync:  off  ·  free Hz".to_string()
                    });
                });
                core_gui::dirty_toggle_row_g(
                    ui,
                    &state.shared.params[P_MOUTH_SYNC],
                    &PARAMS[P_MOUTH_SYNC],
                    &state.shared.dirty_params[P_MOUTH_SYNC],
                    core_gui::GestureBridge {
                        begin: &state.shared.gesture_begin,
                        end: &state.shared.gesture_end,
                    },
                    P_MOUTH_SYNC,
                );
                dirty_param_row(ui, &state.shared, P_MOUTH_DIV);
            });
            core_gui::section(ui, "Timbre", |ui| {
                dirty_param_row(ui, &state.shared, P_BRIGHT);
                dirty_param_row(ui, &state.shared, P_TONGUE_ST);
            });
            core_gui::section(ui, "Envelope", |ui| {
                dirty_param_row(ui, &state.shared, P_ATTACK);
                dirty_param_row(ui, &state.shared, P_DECAY);
                dirty_param_row(ui, &state.shared, P_SUSTAIN);
                dirty_param_row(ui, &state.shared, P_RELEASE);
            });
            core_gui::section(ui, "Output", |ui| {
                dirty_param_row(ui, &state.shared, P_OUTPUT);
            });
            core_gui::help_block(
                ui,
                "kubyz_help",
                &[
                    (
                        "What it is",
                        "Physical-model jaw harp / khomus. 16-harmonic additive engine \
                         shaped by a 3-band formant filter you drive with an IPA vowel \
                         pad. Played by MIDI; pitch = drone note, articulation = mouth \
                         trajectory.",
                    ),
                    (
                        "Mouth pad (IPA)",
                        "Drag the cursor on the vowel grid to morph between F1/F2/F3 \
                         resonances of real vowel formants (Peterson-Barney male \
                         averages). Snap targets: ɑ, ɛ, i, o, u. Different vowels = \
                         different timbres on the same note.",
                    ),
                    (
                        "Mouth motion",
                        "Mouth Shape (Circle/Sine/Figure-8/Triangle/Line) defines the \
                         trajectory of the auto-mouth motion. Mouth Rate sets speed \
                         (or sync to host BPM with `M Sync`). Mouth Depth = how much \
                         the trajectory moves through formant space. Mouth Stereo \
                         pans the motion left/right for a wider field.",
                    ),
                    (
                        "Presets",
                        "Bashkir — folk khomus tuning, breathy. Khomus — Yakutian metal \
                         jaw harp, hard timbre. Real-D2 — analysed from a real recording \
                         in D2. Tweak with the Mouth pad first, formant gain second, \
                         envelope last.",
                    ),
                    (
                        "CC mapping",
                        "CC 1 → Mouth Depth, CC 2 → Mouth Stereo, CC 11 → F1, CC 71 → \
                         F3, CC 74 → F2. Send Modulation+Expression from a foot \
                         controller for hands-free morphing while playing.",
                    ),
                ],
            );
        });
    });
}
