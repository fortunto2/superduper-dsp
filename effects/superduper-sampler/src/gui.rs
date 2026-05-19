//! GUI for SuperDuper Sampler — sample picker dropdown + ADSR knobs.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::{
    pick_sample, refresh_library, P_ATTACK, P_DECAY, P_FINE, P_LOOP, P_LOOP_END, P_LOOP_START,
    P_OUTPUT, P_RELEASE, P_ROOT, P_SUSTAIN, P_TUNE, PARAMS, SharedParams,
};

pub const DEFAULT_WIDTH: u32 = 640;
pub const DEFAULT_HEIGHT: u32 = 520;
pub const MIN_WIDTH: u32 = 480;
pub const MIN_HEIGHT: u32 = 400;
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
    /// Last status message — set by Pick / Refresh actions.
    status: String,
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P, shared: SharedParams, resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Sampler".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let state = GuiState {
        shared, resize,
        applied_size: (initial_w, initial_h),
        status: "Pick a sample from the dropdown ↓".into(),
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
        // Skip the preset combo on the top bar (we use it for samples)
        let _ = core_gui::top_bar(
            ui, "SuperDuper Sampler",
            env!("SDSP_BUILD_NUM"), env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "sampler_dummy_combo", &[""][..], &mut None::<usize>,
        );
        core_gui::ab_init_bar(
            ui, &state.shared.ab_snapshot,
            &state.shared.params, PARAMS, &state.shared.dirty_params,
        );

        let (scope_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 56.0),
            egui::Sense::hover(),
        );
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, scope_rect, 48_000.0);

        ui.add_space(6.0);

        // Sample picker. The library lock is held briefly to copy
        // names and the current index — no audio-thread blocking.
        let (names, current_idx) = {
            let lib = state.shared.library.lock();
            let names: Vec<String> = lib.iter()
                .map(|p| p.file_stem().map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "?".into()))
                .collect();
            let idx = state.shared.current_index.load(Ordering::Relaxed);
            (names, idx)
        };

        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Sample:").color(core_gui::GREEN_BRIGHT).monospace());
            let selected_name = if current_idx >= 0 && (current_idx as usize) < names.len() {
                names[current_idx as usize].clone()
            } else {
                "(pick one)".into()
            };
            egui::ComboBox::from_id_salt("sampler_sample_combo")
                .selected_text(selected_name)
                .show_ui(ui, |ui| {
                    for (i, n) in names.iter().enumerate() {
                        if ui.selectable_label(current_idx as usize == i, n).clicked() {
                            match pick_sample(&state.shared, i) {
                                Ok(name) => state.status = format!("Loaded: {}", name),
                                Err(e) => state.status = format!("Load failed: {}", e),
                            }
                        }
                    }
                });
            if ui.button("Rescan").clicked() {
                let c = refresh_library(&state.shared);
                state.status = format!("Scanned: {} samples found", c);
            }
        });

        ui.label(egui::RichText::new(&state.status).color(core_gui::GREEN_DIM).monospace().small());
        ui.label(egui::RichText::new(
            "Sample folders: ~/Music/SuperDuper Samples/ + ~/Music/Favorite 808s/")
            .color(core_gui::GREEN_DIM).monospace().small());

        ui.add_space(8.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Pitch", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ROOT], &PARAMS[P_ROOT], &state.shared.dirty_params[P_ROOT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ROOT);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_TUNE], &PARAMS[P_TUNE], &state.shared.dirty_params[P_TUNE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_TUNE);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_FINE], &PARAMS[P_FINE], &state.shared.dirty_params[P_FINE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_FINE);
            });
            core_gui::section(ui, "Loop", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LOOP], &PARAMS[P_LOOP], &state.shared.dirty_params[P_LOOP], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LOOP);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LOOP_START], &PARAMS[P_LOOP_START], &state.shared.dirty_params[P_LOOP_START], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LOOP_START);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LOOP_END], &PARAMS[P_LOOP_END], &state.shared.dirty_params[P_LOOP_END], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LOOP_END);
            });
            core_gui::section(ui, "Envelope", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ATTACK], &PARAMS[P_ATTACK], &state.shared.dirty_params[P_ATTACK], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ATTACK);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_DECAY], &PARAMS[P_DECAY], &state.shared.dirty_params[P_DECAY], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_DECAY);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_SUSTAIN], &PARAMS[P_SUSTAIN], &state.shared.dirty_params[P_SUSTAIN], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_SUSTAIN);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_RELEASE], &PARAMS[P_RELEASE], &state.shared.dirty_params[P_RELEASE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_RELEASE);
            });
            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_OUTPUT);
            });
        });
    });
}
