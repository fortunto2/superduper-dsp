//! GUI for SuperDuper EQ.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{P_HIGH_FREQ, P_HIGH_GAIN, P_HP, P_LOW_FREQ, P_LOW_GAIN, P_LP, P_MID_FREQ, P_MID_GAIN,
            P_MID_Q, P_OUTPUT, PARAMS, SharedParams};
use std::sync::atomic::Ordering;
use superduper_synth_core::dsp_blocks::Biquad;

pub const DEFAULT_WIDTH: u32 = 520;
pub const DEFAULT_HEIGHT: u32 = 500;
pub const MIN_WIDTH: u32 = 400;
pub const MIN_HEIGHT: u32 = 380;
pub const MAX_WIDTH: u32 = 1400;
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
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P, shared: SharedParams, resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper EQ".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let state = GuiState {
        shared, resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(0),
        preset_names: PRESETS.iter().map(|p| p.name).collect(),
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
            ui, "SuperDuper EQ",
            env!("SDSP_BUILD_NUM"), env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "eq_preset_combo", &state.preset_names, &mut state.selected_preset,
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
        let (sdsp_scope_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 140.0),
            egui::Sense::hover(),
        );
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, sdsp_scope_rect, 48_000.0);
        draw_eq_curve_overlay(ui, &state.shared, sdsp_scope_rect, 48_000.0);
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Filters", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_HP], &PARAMS[P_HP], &state.shared.dirty_params[P_HP], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_HP);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LP], &PARAMS[P_LP], &state.shared.dirty_params[P_LP], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LP);
            });
            core_gui::section(ui, "Low Shelf", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LOW_FREQ], &PARAMS[P_LOW_FREQ], &state.shared.dirty_params[P_LOW_FREQ], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LOW_FREQ);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LOW_GAIN], &PARAMS[P_LOW_GAIN], &state.shared.dirty_params[P_LOW_GAIN], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LOW_GAIN);
            });
            core_gui::section(ui, "Mid Peak", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MID_FREQ], &PARAMS[P_MID_FREQ], &state.shared.dirty_params[P_MID_FREQ], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MID_FREQ);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MID_GAIN], &PARAMS[P_MID_GAIN], &state.shared.dirty_params[P_MID_GAIN], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MID_GAIN);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MID_Q], &PARAMS[P_MID_Q], &state.shared.dirty_params[P_MID_Q], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MID_Q);
            });
            core_gui::section(ui, "High Shelf", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_HIGH_FREQ], &PARAMS[P_HIGH_FREQ], &state.shared.dirty_params[P_HIGH_FREQ], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_HIGH_FREQ);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_HIGH_GAIN], &PARAMS[P_HIGH_GAIN], &state.shared.dirty_params[P_HIGH_GAIN], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_HIGH_GAIN);
            });
            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_OUTPUT);
            });
        });
    });
}

/// Compute the combined EQ frequency response in dB and draw it as a
/// bright polyline on top of the existing spectrum strip. Visualises
/// the curve the user is shaping — Pro-Q / FabFilter feel.
fn draw_eq_curve_overlay(
    ui: &mut egui::Ui,
    shared: &SharedParams,
    rect: egui::Rect,
    sr: f32,
) {
    let painter = ui.painter_at(rect);
    // Reconstruct the EQ chain biquads from the live param values.
    let mut biquads: Vec<Biquad> = Vec::with_capacity(5);
    let hp_freq = shared.params[P_HP].load(Ordering::Relaxed);
    if hp_freq > 1.0 {
        let mut b = Biquad::default();
        b.set_hpf(sr, hp_freq.clamp(20.0, sr * 0.49), 0.707);
        biquads.push(b);
    }
    let mut low = Biquad::default();
    low.set_low_shelf(
        sr,
        shared.params[P_LOW_FREQ].load(Ordering::Relaxed),
        1.0,
        shared.params[P_LOW_GAIN].load(Ordering::Relaxed),
    );
    biquads.push(low);
    let mut mid = Biquad::default();
    mid.set_peaking(
        sr,
        shared.params[P_MID_FREQ].load(Ordering::Relaxed),
        shared.params[P_MID_Q].load(Ordering::Relaxed),
        shared.params[P_MID_GAIN].load(Ordering::Relaxed),
    );
    biquads.push(mid);
    let mut high = Biquad::default();
    high.set_high_shelf(
        sr,
        shared.params[P_HIGH_FREQ].load(Ordering::Relaxed),
        1.0,
        shared.params[P_HIGH_GAIN].load(Ordering::Relaxed),
    );
    biquads.push(high);
    let lp_freq = shared.params[P_LP].load(Ordering::Relaxed);
    if lp_freq > 100.0 && lp_freq < sr * 0.49 {
        let mut b = Biquad::default();
        b.set_lpf(sr, lp_freq, 0.707);
        biquads.push(b);
    }
    let out_db = shared.params[P_OUTPUT].load(Ordering::Relaxed);

    // Y mapping: ±18 dB across the strip's vertical extent.
    let centre_y = rect.center().y;
    let y_scale = rect.height() * 0.45 / 18.0;
    let to_y = |db: f32| centre_y - db.clamp(-18.0, 18.0) * y_scale;

    // X mapping: log 20 Hz – 20 kHz.
    let log_lo = 20.0_f32.log10();
    let log_hi = 20_000.0_f32.log10();
    let cols = (rect.width() as usize).max(8).min(800);
    let mut prev: Option<egui::Pos2> = None;
    for i in 0..cols {
        let frac = i as f32 / (cols - 1) as f32;
        let f = 10f32.powf(log_lo + (log_hi - log_lo) * frac);
        let mut db = out_db;
        for b in &biquads {
            db += b.magnitude_db_at(f, sr);
        }
        let x = rect.left() + frac * rect.width();
        let y = to_y(db);
        let pt = egui::pos2(x, y);
        if let Some(p) = prev {
            painter.line_segment([p, pt], egui::Stroke::new(1.5, core_gui::GREEN_BRIGHT));
        }
        prev = Some(pt);
    }

    // 0 dB reference line.
    painter.line_segment(
        [
            egui::pos2(rect.left(), centre_y),
            egui::pos2(rect.right(), centre_y),
        ],
        egui::Stroke::new(0.5, core_gui::GREEN_FAINT),
    );
}
