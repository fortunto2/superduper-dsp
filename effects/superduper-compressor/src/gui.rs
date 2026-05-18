//! GUI for SuperDuper Compressor.
//!
//! Layout, top to bottom:
//!   1. Top bar (preset, bypass, build).
//!   2. **Scope** — live waveform of input (faint) + output (bright),
//!      gain-reduction overlay (magenta) draped over the top, and the
//!      static compression curve (orange) drawn behind both. Modelled
//!      after ZLCompressor's mastering view but rendered in our
//!      phosphor-green theme.
//!   3. Knobs / sliders organised into Compression / Envelope / Detector
//!      / Lookahead / Output sections.

use std::sync::atomic::Ordering;

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use superduper_synth_core::dsp_blocks::{compressor_gain_db_curve, CompressorCurve};
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{
    PARAMS, P_ATTACK, P_AUTO_REL, P_CEILING, P_CURVE, P_HOLD, P_KNEE, P_LINK, P_LOOKAHEAD, P_MS_MODE,
    P_MAKEUP, P_MIX, P_OS, P_RANGE, P_RATIO, P_RELEASE, P_SC_HPF, P_THRESHOLD, SCOPE_LEN,
    SharedParams,
};

pub const DEFAULT_WIDTH: u32 = 760;
pub const DEFAULT_HEIGHT: u32 = 640;
pub const MIN_WIDTH: u32 = 540;
pub const MIN_HEIGHT: u32 = 380;
pub const MAX_WIDTH: u32 = 1600;
pub const MAX_HEIGHT: u32 = 1200;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

const HPF_NAMES: [&str; 4] = ["Off", "80 Hz", "150 Hz", "300 Hz"];
const OS_NAMES: [&str; 3] = ["Off", "2×", "4×"];
const CURVE_NAMES: [&str; 3] = ["Clean", "Pump", "Smooth"];

/// Scope display range (dB). Anything below this floor is clipped to it.
const SCOPE_DB_FLOOR: f32 = -60.0;
const SCOPE_DB_CEIL: f32 = 0.0;
/// GR overlay clipped to this many dB of reduction so the line stays
/// inside the scope panel.
const GR_DISPLAY_RANGE_DB: f32 = 30.0;
/// Width of the input-level histogram strip rendered to the left of the
/// waveform area (in scope-panel pixels).
const HISTOGRAM_WIDTH: f32 = 36.0;
/// Number of dB bins in the histogram. 60 bins over the scope range
/// (-60..0 dB) → 1 dB per bin, fine enough to read mix density without
/// looking jagged.
const HISTOGRAM_BINS: usize = 60;

/// Scope colours — borrowed from the ZLCompressor reference but mapped
/// into the SuperDuper green palette so it doesn't look out of place.
const SCOPE_BG: egui::Color32 = core_gui::PANEL_BG;
/// Input waveform: very low-alpha fill so output / curve / GR lines on
/// top dominate. The fill is there for "sense of mass", not as a
/// readable signal — the input outline (drawn over the fill) is what
/// reads as the input signal.
const INPUT_FILL: egui::Color32 = egui::Color32::from_rgba_premultiplied(14, 26, 18, 35);
const INPUT_LINE: egui::Color32 = egui::Color32::from_rgb(96, 150, 110);
const OUTPUT_LINE: egui::Color32 = egui::Color32::from_rgb(190, 240, 200);
/// Magenta GR overlay — same colour ZLCompressor uses.
const GR_LINE: egui::Color32 = egui::Color32::from_rgb(240, 96, 210);
const CURVE_LINE: egui::Color32 = egui::Color32::from_rgb(240, 178, 92);
const GRID: egui::Color32 = core_gui::GREEN_FAINT;

struct GuiState {
    shared: SharedParams,
    resize: ResizeBridge,
    applied_size: (u32, u32),
    selected_preset: Option<usize>,
    preset_names: Vec<&'static str>,
    /// Toggle for the scope panel — collapsing it hides the waveform,
    /// histogram, curve plot, and GR meter so the window can shrink to
    /// just the param sliders. GUI-local state, doesn't round-trip
    /// through CLAP params on purpose: it's a viewing preference, not
    /// part of the patch.
    show_scope: bool,
    /// Scratch buffers for scope snapshot. Allocated once at GUI build
    /// time so per-frame draws don't churn the allocator. Triple buffer
    /// matches the audio side's ScopeBuf layout (input / output / GR).
    scope_in: Vec<f32>,
    scope_out: Vec<f32>,
    scope_gr: Vec<f32>,
    /// Per-frame polygon scratch — cleared and re-filled each draw rather
    /// than reallocated. Keeps the GUI thread off the allocator at
    /// 30 Hz × ~1024 points.
    curve_pts: Vec<egui::Pos2>,
    input_poly: Vec<egui::Pos2>,
    output_pts: Vec<egui::Pos2>,
    gr_pts: Vec<egui::Pos2>,
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P,
    shared: SharedParams,
    resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Compressor".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let state = GuiState {
        shared,
        resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(0),
        preset_names: PRESETS.iter().map(|p| p.name).collect(),
        show_scope: true,
        scope_in: vec![SCOPE_DB_FLOOR; SCOPE_LEN],
        scope_out: vec![SCOPE_DB_FLOOR; SCOPE_LEN],
        scope_gr: vec![0.0; SCOPE_LEN],
        curve_pts: Vec::with_capacity(65),
        input_poly: Vec::with_capacity(SCOPE_LEN + 2),
        output_pts: Vec::with_capacity(SCOPE_LEN),
        gr_pts: Vec::with_capacity(SCOPE_LEN),
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
            "SuperDuper Compressor",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "comp_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            if let Some(preset) = PRESETS.get(i) {
                crate::presets::apply(&state.shared, preset);
            }

        core_gui::ab_init_bar(
            ui,
            &state.shared.ab_snapshot,
            &state.shared.params,
            PARAMS,
            &state.shared.dirty_params,
        );
        // Compressor has its own dedicated scope view further down — no
        // spectrum strip here.
        }

        // Toggle row — scope on/off + numeric GR readout next to it so
        // users who collapse the scope still see compression activity.
        ui.horizontal(|ui| {
            let label = if state.show_scope { "[X] scope" } else { "[ ] scope" };
            if ui
                .selectable_label(
                    state.show_scope,
                    egui::RichText::new(label).color(core_gui::GREEN).monospace(),
                )
                .clicked()
            {
                state.show_scope = !state.show_scope;
            }
            ui.add_space(8.0);
            let gr_db = state.shared.gain_reduction_db.load(Ordering::Relaxed);
            ui.label(
                egui::RichText::new(format!("{:>5.1} dB GR", gr_db.max(-24.0).min(0.0)))
                    .color(core_gui::GREEN_BRIGHT)
                    .monospace(),
            );
        });

        if state.show_scope {
            draw_scope(ui, state);
            ui.add_space(2.0);
            draw_gr_meter(ui, &state.shared);
            ui.add_space(4.0);
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Compression", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_THRESHOLD], &PARAMS[P_THRESHOLD], &state.shared.dirty_params[P_THRESHOLD], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_THRESHOLD);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_RATIO], &PARAMS[P_RATIO], &state.shared.dirty_params[P_RATIO], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_RATIO);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_KNEE], &PARAMS[P_KNEE], &state.shared.dirty_params[P_KNEE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_KNEE);
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [90.0, 18.0],
                        egui::Label::new(
                            egui::RichText::new("Curve").color(core_gui::GREEN).monospace(),
                        ),
                    );
                    let cur = state.shared.params[P_CURVE]
                        .load(Ordering::Relaxed)
                        .round() as usize;
                    let cur = cur.min(CURVE_NAMES.len() - 1);
                    egui::ComboBox::from_id_salt("comp_curve_combo")
                        .selected_text(CURVE_NAMES[cur])
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for (i, name) in CURVE_NAMES.iter().enumerate() {
                                if ui.selectable_label(cur == i, *name).clicked() {
                                    state.shared.params[P_CURVE]
                                        .store(i as f32, Ordering::Relaxed);
                                }
                            }
                        });
                });
            });

            core_gui::section(ui, "Envelope", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ATTACK], &PARAMS[P_ATTACK], &state.shared.dirty_params[P_ATTACK], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ATTACK);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_HOLD], &PARAMS[P_HOLD], &state.shared.dirty_params[P_HOLD], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_HOLD);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_RELEASE], &PARAMS[P_RELEASE], &state.shared.dirty_params[P_RELEASE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_RELEASE);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_RANGE], &PARAMS[P_RANGE], &state.shared.dirty_params[P_RANGE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_RANGE);
                ui.horizontal(|ui| {
                    let on = state.shared.params[P_AUTO_REL].load(Ordering::Relaxed) > 0.5;
                    let label = if on { "[X] auto release" } else { "[ ] auto release" };
                    if ui
                        .selectable_label(
                            on,
                            egui::RichText::new(label).color(core_gui::GREEN).monospace(),
                        )
                        .clicked()
                    {
                        state.shared.params[P_AUTO_REL]
                            .store(if on { 0.0 } else { 1.0 }, Ordering::Relaxed);
                    }
                });
            });

            core_gui::section(ui, "Detector", |ui| {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [90.0, 18.0],
                        egui::Label::new(
                            egui::RichText::new("SC HPF").color(core_gui::GREEN).monospace(),
                        ),
                    );
                    let cur = state.shared.params[P_SC_HPF]
                        .load(Ordering::Relaxed)
                        .round() as usize;
                    let cur = cur.min(HPF_NAMES.len() - 1);
                    egui::ComboBox::from_id_salt("comp_hpf_combo")
                        .selected_text(HPF_NAMES[cur])
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for (i, name) in HPF_NAMES.iter().enumerate() {
                                if ui.selectable_label(cur == i, *name).clicked() {
                                    state.shared.params[P_SC_HPF]
                                        .store(i as f32, Ordering::Relaxed);
                                }
                            }
                        });
                });
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LINK], &PARAMS[P_LINK], &state.shared.dirty_params[P_LINK], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LINK);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MS_MODE], &PARAMS[P_MS_MODE], &state.shared.dirty_params[P_MS_MODE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MS_MODE);
            });

            core_gui::section(ui, "Lookahead", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LOOKAHEAD], &PARAMS[P_LOOKAHEAD], &state.shared.dirty_params[P_LOOKAHEAD], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LOOKAHEAD);
            });

            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MAKEUP], &PARAMS[P_MAKEUP], &state.shared.dirty_params[P_MAKEUP], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MAKEUP);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_CEILING], &PARAMS[P_CEILING], &state.shared.dirty_params[P_CEILING], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_CEILING);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MIX], &PARAMS[P_MIX], &state.shared.dirty_params[P_MIX], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MIX);
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [90.0, 18.0],
                        egui::Label::new(
                            egui::RichText::new("Oversamp").color(core_gui::GREEN).monospace(),
                        ),
                    );
                    let cur = state.shared.params[P_OS]
                        .load(Ordering::Relaxed)
                        .round() as usize;
                    let cur = cur.min(OS_NAMES.len() - 1);
                    egui::ComboBox::from_id_salt("comp_os_combo")
                        .selected_text(OS_NAMES[cur])
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for (i, name) in OS_NAMES.iter().enumerate() {
                                if ui.selectable_label(cur == i, *name).clicked() {
                                    state.shared.params[P_OS]
                                        .store(i as f32, Ordering::Relaxed);
                                }
                            }
                        });
                });
            });
        });
    });
}

// ---------------------------------------------------------------------------
// Scope drawing — input/output waveform + GR overlay + static curve
// ---------------------------------------------------------------------------

fn draw_scope(ui: &mut egui::Ui, state: &mut GuiState) {
    // Refresh scope snapshot. Audio thread writes in real-time; we copy
    // an in-order chronological snapshot once per frame.
    state.shared.scope.snapshot_in_order(
        &mut state.scope_in,
        &mut state.scope_out,
        &mut state.scope_gr,
    );

    let outer = egui::vec2(ui.available_width().min(1200.0), 170.0);
    let (full_rect, _resp) = ui.allocate_exact_size(outer, egui::Sense::hover());
    let painter = ui.painter_at(full_rect);

    // Split: histogram strip on the left, waveform on the right.
    let hist_rect = egui::Rect::from_min_size(
        full_rect.min,
        egui::vec2(HISTOGRAM_WIDTH, full_rect.height()),
    );
    let rect = egui::Rect::from_min_size(
        egui::pos2(full_rect.min.x + HISTOGRAM_WIDTH + 2.0, full_rect.min.y),
        egui::vec2(full_rect.width() - HISTOGRAM_WIDTH - 2.0, full_rect.height()),
    );

    painter.rect_filled(full_rect, 0.0, SCOPE_BG);
    painter.rect_stroke(
        full_rect,
        0.0,
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
        egui::StrokeKind::Inside,
    );
    draw_histogram(&painter, hist_rect, &state.scope_in);

    // Horizontal grid every 12 dB.
    for db in [-12, -24, -36, -48] {
        let frac = (db as f32 - SCOPE_DB_FLOOR) / (SCOPE_DB_CEIL - SCOPE_DB_FLOOR);
        let y = rect.bottom() - rect.height() * frac;
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            egui::Stroke::new(1.0, GRID),
        );
        painter.text(
            egui::pos2(rect.right() - 4.0, y),
            egui::Align2::RIGHT_BOTTOM,
            format!("{db}"),
            egui::FontId::monospace(9.0),
            core_gui::GREEN_DIM,
        );
    }

    // Static curve plot — draw before the live waveform so the moving
    // signal sits on top. X axis = input dB (mapped left-to-right over
    // the scope's range), Y axis = output dB (linear gain reduction
    // mapped onto the same dB span).
    let threshold = state.shared.params[P_THRESHOLD].load(Ordering::Relaxed);
    let ratio = state.shared.params[P_RATIO].load(Ordering::Relaxed);
    let knee = state.shared.params[P_KNEE].load(Ordering::Relaxed);
    let makeup = state.shared.params[P_MAKEUP].load(Ordering::Relaxed);
    let curve_kind = CompressorCurve::from_index(
        state.shared.params[P_CURVE].load(Ordering::Relaxed).round() as u32,
    );
    // Draw order: input fill (background mass), curve (orange), input
    // outline (faint contour), output line (bright), GR line (top). All
    // line layers above the fill so they read clearly against the wave.
    let n = state.scope_in.len();
    let dx = rect.width() / n as f32;
    state.input_poly.clear();
    state.input_poly.push(egui::pos2(rect.left(), rect.bottom()));
    for (i, db) in state.scope_in.iter().enumerate() {
        let v = db.clamp(SCOPE_DB_FLOOR, SCOPE_DB_CEIL);
        let frac = (v - SCOPE_DB_FLOOR) / (SCOPE_DB_CEIL - SCOPE_DB_FLOOR);
        let y = rect.bottom() - rect.height() * frac;
        let x = rect.left() + dx * i as f32;
        state.input_poly.push(egui::pos2(x, y));
    }
    state.input_poly.push(egui::pos2(rect.right(), rect.bottom()));
    painter.add(egui::Shape::Path(egui::epaint::PathShape {
        points: state.input_poly.clone(),
        closed: true,
        fill: INPUT_FILL,
        stroke: egui::Stroke::NONE.into(),
    }));

    state.curve_pts.clear();
    for i in 0..=64_usize {
        let in_db = SCOPE_DB_FLOOR + (SCOPE_DB_CEIL - SCOPE_DB_FLOOR) * (i as f32 / 64.0);
        let gr = compressor_gain_db_curve(in_db, threshold, ratio, knee, curve_kind);
        let out_db = (in_db + gr + makeup).clamp(SCOPE_DB_FLOOR, SCOPE_DB_CEIL);
        let x = rect.left() + rect.width() * (i as f32 / 64.0);
        let y = rect.bottom() - rect.height() * (out_db - SCOPE_DB_FLOOR)
            / (SCOPE_DB_CEIL - SCOPE_DB_FLOOR);
        state.curve_pts.push(egui::pos2(x, y));
    }
    painter.add(egui::Shape::line(
        state.curve_pts.clone(),
        egui::Stroke::new(2.0, CURVE_LINE),
    ));

    // Input contour — separate from the fill so we can layer it after
    // the curve (so the input outline doesn't bury the orange).
    let input_outline: Vec<egui::Pos2> = state.input_poly[1..state.input_poly.len() - 1].to_vec();
    painter.add(egui::Shape::line(
        input_outline,
        egui::Stroke::new(1.0, INPUT_LINE),
    ));

    state.output_pts.clear();
    for (i, db) in state.scope_out.iter().enumerate() {
        let v = db.clamp(SCOPE_DB_FLOOR, SCOPE_DB_CEIL);
        let frac = (v - SCOPE_DB_FLOOR) / (SCOPE_DB_CEIL - SCOPE_DB_FLOOR);
        let y = rect.bottom() - rect.height() * frac;
        let x = rect.left() + dx * i as f32;
        state.output_pts.push(egui::pos2(x, y));
    }
    painter.add(egui::Shape::line(
        state.output_pts.clone(),
        egui::Stroke::new(1.8, OUTPUT_LINE),
    ));

    let gr_band_h = (rect.height() * 0.4).min(80.0);
    state.gr_pts.clear();
    for (i, gr_db) in state.scope_gr.iter().enumerate() {
        let g = (-gr_db).clamp(0.0, GR_DISPLAY_RANGE_DB);
        let frac = g / GR_DISPLAY_RANGE_DB;
        let y = rect.top() + gr_band_h * frac;
        let x = rect.left() + dx * i as f32;
        state.gr_pts.push(egui::pos2(x, y));
    }
    painter.add(egui::Shape::line(
        state.gr_pts.clone(),
        egui::Stroke::new(2.2, GR_LINE),
    ));

    // Legend strip at the top of the scope.
    painter.text(
        egui::pos2(rect.left() + 6.0, rect.top() + 2.0),
        egui::Align2::LEFT_TOP,
        "■ in  ▶ out  ▬ gr  ▬ curve",
        egui::FontId::monospace(10.0),
        core_gui::GREEN_DIM,
    );
}

/// Input-level histogram. Bins each scope frame's input dB value into a
/// fixed-resolution grid spanning the scope's dB range, then draws each
/// bin as a horizontal bar — taller bin = more frames sat at that level.
/// ZLCompressor's left strip is the inspiration; this version uses the
/// SuperDuper green palette so it ties into the theme.
fn draw_histogram(painter: &egui::Painter, rect: egui::Rect, in_db: &[f32]) {
    let mut bins = [0u32; HISTOGRAM_BINS];
    for &v in in_db {
        if !v.is_finite() {
            continue;
        }
        let v = v.clamp(SCOPE_DB_FLOOR, SCOPE_DB_CEIL);
        let frac = (v - SCOPE_DB_FLOOR) / (SCOPE_DB_CEIL - SCOPE_DB_FLOOR);
        let idx = (frac * (HISTOGRAM_BINS as f32 - 1.0)).round() as usize;
        bins[idx] = bins[idx].saturating_add(1);
    }
    let max = bins.iter().copied().max().unwrap_or(1).max(1);

    let bin_h = rect.height() / HISTOGRAM_BINS as f32;
    for (i, &count) in bins.iter().enumerate() {
        if count == 0 {
            continue;
        }
        let bar_frac = count as f32 / max as f32;
        let bar_w = rect.width() * bar_frac;
        // Bins are indexed bottom-up: bin 0 = SCOPE_DB_FLOOR.
        let y_bottom = rect.bottom() - bin_h * i as f32;
        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left(), y_bottom - bin_h),
            egui::vec2(bar_w.max(1.0), (bin_h - 0.5).max(1.0)),
        );
        // Brighten taller bars so the "this is where the signal lives"
        // peak pops visually.
        let bright = (0.35 + 0.65 * bar_frac).clamp(0.0, 1.0);
        let r = (INPUT_LINE.r() as f32 * bright) as u8;
        let g = (INPUT_LINE.g() as f32 * bright) as u8;
        let b = (INPUT_LINE.b() as f32 * bright) as u8;
        painter.rect_filled(bar_rect, 0.0, egui::Color32::from_rgb(r, g, b));
    }

    // Right-edge separator so the histogram doesn't bleed into the
    // waveform area visually.
    painter.line_segment(
        [
            egui::pos2(rect.right() + 1.0, rect.top()),
            egui::pos2(rect.right() + 1.0, rect.bottom()),
        ],
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
    );
}

fn draw_gr_meter(ui: &mut egui::Ui, shared: &crate::SharedParamsInner) {
    let gr_db = shared.gain_reduction_db.load(Ordering::Relaxed);
    let display = gr_db.max(-24.0).min(0.0);
    let frac = (-display / 24.0).clamp(0.0, 1.0);

    let desired = egui::vec2(ui.available_width().min(420.0), 22.0);
    let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, 0.0, core_gui::PANEL_BG);
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
        egui::StrokeKind::Inside,
    );

    let fill_w = rect.width() * frac;
    if fill_w > 1.0 {
        let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, rect.height()));
        let bright = (0.4 + frac * 0.6).min(1.0);
        let r = (core_gui::GREEN_BRIGHT.r() as f32 * bright) as u8;
        let g = (core_gui::GREEN_BRIGHT.g() as f32 * bright) as u8;
        let b = (core_gui::GREEN_BRIGHT.b() as f32 * bright) as u8;
        painter.rect_filled(fill_rect, 0.0, egui::Color32::from_rgb(r, g, b));
    }

    for db in (1..=8).map(|i| i * 3) {
        let x = rect.left() + rect.width() * (db as f32 / 24.0);
        painter.line_segment(
            [
                egui::pos2(x, rect.top()),
                egui::pos2(x, rect.top() + 4.0),
            ],
            egui::Stroke::new(1.0, core_gui::GREEN_DIM),
        );
    }

    painter.text(
        egui::pos2(rect.right() - 6.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("{:.1} dB GR", display),
        egui::FontId::monospace(11.0),
        core_gui::GREEN_BRIGHT,
    );
}

