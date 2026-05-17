//! GUI for SuperDuper Compressor — knobs + live gain-reduction meter.

use std::sync::atomic::Ordering;

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{P_ATTACK, P_KNEE, P_MAKEUP, P_MIX, P_RATIO, P_RELEASE, P_SC_HPF, P_THRESHOLD, PARAMS,
            SharedParams};

pub const DEFAULT_WIDTH: u32 = 500;
pub const DEFAULT_HEIGHT: u32 = 460;
pub const MIN_WIDTH: u32 = 380;
pub const MIN_HEIGHT: u32 = 360;
pub const MAX_WIDTH: u32 = 1400;
pub const MAX_HEIGHT: u32 = 1100;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

const HPF_NAMES: [&str; 4] = ["Off", "80 Hz", "150 Hz", "300 Hz"];

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
        }

        // Gain reduction meter — block-rate updated by audio thread.
        draw_gr_meter(ui, &state.shared);
        ui.add_space(4.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Compression", |ui| {
                core_gui::param_row(ui, &state.shared.params[P_THRESHOLD], &PARAMS[P_THRESHOLD]);
                core_gui::param_row(ui, &state.shared.params[P_RATIO], &PARAMS[P_RATIO]);
                core_gui::param_row(ui, &state.shared.params[P_KNEE], &PARAMS[P_KNEE]);
            });

            core_gui::section(ui, "Envelope", |ui| {
                core_gui::param_row(ui, &state.shared.params[P_ATTACK], &PARAMS[P_ATTACK]);
                core_gui::param_row(ui, &state.shared.params[P_RELEASE], &PARAMS[P_RELEASE]);
            });

            core_gui::section(ui, "Detector", |ui| {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [90.0, 18.0],
                        egui::Label::new(
                            egui::RichText::new("SC HPF").color(core_gui::GREEN).monospace(),
                        ),
                    );
                    let cur = state.shared.params[P_SC_HPF].load(Ordering::Relaxed).round() as usize;
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
            });

            core_gui::section(ui, "Output", |ui| {
                core_gui::param_row(ui, &state.shared.params[P_MAKEUP], &PARAMS[P_MAKEUP]);
                core_gui::param_row(ui, &state.shared.params[P_MIX], &PARAMS[P_MIX]);
            });
        });
    });
}

fn draw_gr_meter(ui: &mut egui::Ui, shared: &crate::SharedParamsInner) {
    let gr_db = shared.gain_reduction_db.load(Ordering::Relaxed);
    let display = gr_db.max(-24.0).min(0.0);
    let frac = (-display / 24.0).clamp(0.0, 1.0);

    let desired = egui::vec2(ui.available_width().min(420.0), 22.0);
    let (rect, _resp) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let painter = ui.painter_at(rect);

    // Background.
    painter.rect_filled(rect, 0.0, core_gui::PANEL_BG);
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
        egui::StrokeKind::Inside,
    );

    // Filled meter — width proportional to gain reduction.
    let fill_w = rect.width() * frac;
    if fill_w > 1.0 {
        let fill_rect = egui::Rect::from_min_size(
            rect.min,
            egui::vec2(fill_w, rect.height()),
        );
        // Brightness scales with how much reduction we're applying.
        let bright = (0.4 + frac * 0.6).min(1.0);
        let r = (core_gui::GREEN_BRIGHT.r() as f32 * bright) as u8;
        let g = (core_gui::GREEN_BRIGHT.g() as f32 * bright) as u8;
        let b = (core_gui::GREEN_BRIGHT.b() as f32 * bright) as u8;
        painter.rect_filled(fill_rect, 0.0, egui::Color32::from_rgb(r, g, b));
    }

    // Tick marks every 3 dB.
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

    // Numeric readout.
    painter.text(
        egui::pos2(rect.right() - 6.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        format!("{:.1} dB GR", display),
        egui::FontId::monospace(11.0),
        core_gui::GREEN_BRIGHT,
    );
}
