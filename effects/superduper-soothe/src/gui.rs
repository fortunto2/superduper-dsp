use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{
    N_BANDS, PARAMS, P_AMOUNT, P_ATTACK, P_HI, P_LO, P_MIX, P_MODE, P_OUTPUT, P_Q, P_RELEASE,
    P_SENS, SharedParams,
};

pub const DEFAULT_WIDTH: u32 = 640;
pub const DEFAULT_HEIGHT: u32 = 560;
pub const MIN_WIDTH: u32 = 480;
pub const MIN_HEIGHT: u32 = 400;
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
        title: "SuperDuper Soothe".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let preset_idx =
        (shared.active_preset.load(Ordering::Relaxed) as usize).min(PRESETS.len().saturating_sub(1));
    let state = GuiState {
        shared,
        resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(preset_idx),
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
            "SuperDuper Soothe",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "soothe_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            if let Some(preset) = PRESETS.get(i) {
                crate::presets::apply(&state.shared, preset);
                state.shared.active_preset.store(i as u32, Ordering::Relaxed);
            }
        }

        core_gui::ab_init_bar(
            ui,
            &state.shared.ab_snapshot,
            &state.shared.params,
            PARAMS,
            &state.shared.dirty_params,
        );

        // Spectrum + per-band cut overlay — taller than the default
        // because Soothe's main story is "see what got grabbed".
        let (scope_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 120.0),
            egui::Sense::hover(),
        );
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, scope_rect, 48_000.0);

        // Paint per-band cut bars hanging from the top of the strip.
        // Deeper bar = more reduction at that band's centre frequency.
        let painter = ui.painter_at(scope_rect);
        let max_cut = state.shared.params[P_AMOUNT].load(Ordering::Relaxed).max(1.0);
        let cut_colour = egui::Color32::from_rgb(230, 100, 90);
        let mut hottest = (0usize, 0.0f32);
        for b in 0..N_BANDS {
            let f = state.shared.band_freq_hz[b].load(Ordering::Relaxed);
            let cut = state.shared.band_cut_db[b].load(Ordering::Relaxed); // negative or zero
            let depth = (-cut).clamp(0.0, max_cut);
            if depth > hottest.1 {
                hottest = (b, depth);
            }
            if depth < 0.05 {
                continue;
            }
            let t = (f.clamp(20.0, 20_000.0).log10() - 20f32.log10())
                / (20_000f32.log10() - 20f32.log10());
            let x = scope_rect.left() + t * scope_rect.width();
            let h = (depth / max_cut) * scope_rect.height();
            let bar = egui::Rect::from_min_max(
                egui::pos2(x - 3.0, scope_rect.top()),
                egui::pos2(x + 3.0, scope_rect.top() + h),
            );
            painter.rect_filled(
                bar,
                0.0,
                egui::Color32::from_rgba_unmultiplied(
                    cut_colour.r(),
                    cut_colour.g(),
                    cut_colour.b(),
                    180,
                ),
            );
        }
        // Caption on the hottest band so the user can read what's
        // currently getting suppressed without squinting at all 24 bars.
        if hottest.1 > 0.05 {
            let f = state.shared.band_freq_hz[hottest.0].load(Ordering::Relaxed);
            core_gui::draw_spectrum_marker_colored(
                ui,
                scope_rect,
                "Cut",
                f,
                -hottest.1,
                cut_colour,
                true,
            );
        }

        ui.add_space(4.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            let g = || core_gui::GestureBridge {
                begin: &state.shared.gesture_begin,
                end: &state.shared.gesture_end,
            };
            core_gui::section(ui, "Suppression", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_AMOUNT], &PARAMS[P_AMOUNT], &state.shared.dirty_params[P_AMOUNT], g(), P_AMOUNT);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_SENS], &PARAMS[P_SENS], &state.shared.dirty_params[P_SENS], g(), P_SENS);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_Q], &PARAMS[P_Q], &state.shared.dirty_params[P_Q], g(), P_Q);
                const MODE_NAMES: [&str; 3] = ["Soft", "Sharp", "Hard"];
                core_gui::dirty_choice_row_g(ui, &state.shared.params[P_MODE], &PARAMS[P_MODE], &MODE_NAMES, &state.shared.dirty_params[P_MODE], g(), P_MODE);
            });
            core_gui::section(ui, "Spectral Range", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LO], &PARAMS[P_LO], &state.shared.dirty_params[P_LO], g(), P_LO);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_HI], &PARAMS[P_HI], &state.shared.dirty_params[P_HI], g(), P_HI);
            });
            core_gui::section(ui, "Dynamics", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ATTACK], &PARAMS[P_ATTACK], &state.shared.dirty_params[P_ATTACK], g(), P_ATTACK);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_RELEASE], &PARAMS[P_RELEASE], &state.shared.dirty_params[P_RELEASE], g(), P_RELEASE);
            });
            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MIX], &PARAMS[P_MIX], &state.shared.dirty_params[P_MIX], g(), P_MIX);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT], g(), P_OUTPUT);
            });
            core_gui::help_block(
                ui,
                "soothe_help",
                &[
                    (
                        "What it does",
                        "24-band filter bank measures the energy in each log-spaced band \
                         and compares it to a sliding average of its 4 neighbours \
                         (\"baseline\"). When a band sticks out more than `Sens` dB above \
                         baseline, the plugin drops a dynamic peaking-EQ cut at that band's \
                         centre. Cut depth scales with the excess up to `Amount`.",
                    ),
                    (
                        "Tuning",
                        "Start with the `Russian Voice` or `Vocal Resonance` preset. \
                         If too much body is being shaved — narrow the Lo/Hi range or \
                         raise Sens. If resonances still leak through — lower Sens or \
                         raise Amount. Higher Q = surgical / narrow cuts; lower Q = wide / \
                         transparent.",
                    ),
                    (
                        "Modes",
                        "Soft (0.4×) — gentle, only the hottest peaks pull. Sharp (0.7×, \
                         default) — leans into peaks but stays musical. Hard (1.0×) — \
                         every dB above baseline = a dB cut. Mastering material → Soft. \
                         Aggressive vocal cleanup → Hard.",
                    ),
                    (
                        "Spectrum readout",
                        "Red bars hanging from the top of the spectrum strip show \
                         per-band cut depth in real time. The hottest band gets a \
                         floating `Cut <freq> · <dB>` label.",
                    ),
                ],
            );
        });
    });
}
