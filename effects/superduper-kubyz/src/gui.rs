use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::{presets, N_HARMONICS};
use crate::{
    apply_preset, PARAMS, P_ATTACK, P_BRIGHT, P_DECAY, P_F1, P_F2, P_F3, P_OUTPUT, P_RELEASE,
    P_SUSTAIN, P_TONGUE_ST, P_VEL_SHIFT, P_VOX_MIX, SharedParams,
};

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
    let state = GuiState {
        shared,
        resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(0),
        preset_names,
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
            shared.params[P_F1].store(f1, Ordering::Relaxed);
            shared.params[P_F2].store(f2, Ordering::Relaxed);
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
            shared.params[P_F3].store(f3, Ordering::Relaxed);
        }
    }

    // Cursor — bright green dot at the current F1/F2 read straight from
    // the atomics (lets external automation move it too).
    let cur_f1 = shared.params[P_F1].load(Ordering::Relaxed);
    let cur_f2 = shared.params[P_F2].load(Ordering::Relaxed);
    let cur_p = to_screen(cur_f1, cur_f2);
    painter.circle_filled(cur_p, 7.0, core_gui::GREEN_BRIGHT);
    painter.circle_stroke(cur_p, 9.0, egui::Stroke::new(1.0, core_gui::GREEN));

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
            }
        }

        let active = state.shared.active_voices.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            ui.label(format!("Voices: {active} / {}", crate::VOICE_COUNT));
            ui.add_space(12.0);
            ui.weak("drag bars to scribble harmonics  ·  click = set");
        });
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
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 160.0),
                    egui::Sense::hover(),
                );
                draw_vowel_pad(ui, &state.shared, rect);
            });
            core_gui::section(ui, "Formant (fine)", |ui| {
                core_gui::param_row(ui, &state.shared.params[P_F1], &PARAMS[P_F1]);
                core_gui::param_row(ui, &state.shared.params[P_F2], &PARAMS[P_F2]);
                core_gui::param_row(ui, &state.shared.params[P_F3], &PARAMS[P_F3]);
                core_gui::param_row(ui, &state.shared.params[P_VOX_MIX], &PARAMS[P_VOX_MIX]);
                core_gui::param_row(ui, &state.shared.params[P_VEL_SHIFT], &PARAMS[P_VEL_SHIFT]);
            });
            core_gui::section(ui, "Timbre", |ui| {
                core_gui::param_row(ui, &state.shared.params[P_BRIGHT], &PARAMS[P_BRIGHT]);
                core_gui::param_row(ui, &state.shared.params[P_TONGUE_ST], &PARAMS[P_TONGUE_ST]);
            });
            core_gui::section(ui, "Envelope", |ui| {
                core_gui::param_row(ui, &state.shared.params[P_ATTACK], &PARAMS[P_ATTACK]);
                core_gui::param_row(ui, &state.shared.params[P_DECAY], &PARAMS[P_DECAY]);
                core_gui::param_row(ui, &state.shared.params[P_SUSTAIN], &PARAMS[P_SUSTAIN]);
                core_gui::param_row(ui, &state.shared.params[P_RELEASE], &PARAMS[P_RELEASE]);
            });
            core_gui::section(ui, "Output", |ui| {
                core_gui::param_row(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT]);
            });
        });
    });
}
