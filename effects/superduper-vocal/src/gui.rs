use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{
    PARAMS, P_CLK_AMT, P_CLK_FLOOR, P_CLK_SENS, P_ESS_AMT, P_ESS_FREQ, P_ESS_LISTEN, P_ESS_RANGE,
    P_ESS_THR, P_ESS_TRACK, P_EXT_KEY, P_HUM_FREQ, P_HUM_ON, P_HUM_STR, P_LO_AMT, P_LO_FREQ,
    P_LO_THR, P_MIX, P_OUTPUT, P_PLOS_AMT, P_PLOS_FREQ, P_PLOS_ON, P_PLOS_THR, P_SUB_MODE,
    SharedParams,
};

pub const DEFAULT_WIDTH: u32 = 560;
pub const DEFAULT_HEIGHT: u32 = 540;
pub const MIN_WIDTH: u32 = 420;
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
        title: "SuperDuper Vocal".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let initial_preset_idx = (shared.active_preset.load(std::sync::atomic::Ordering::Relaxed) as usize)
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
            "SuperDuper Vocal",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "vocal_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            if let Some(preset) = PRESETS.get(i) {
                crate::presets::apply(&state.shared, preset);
                state.shared.active_preset.store(i as u32, std::sync::atomic::Ordering::Relaxed);
            }
        }

        core_gui::ab_init_bar(
            ui,
            &state.shared.ab_snapshot,
            &state.shared.params,
            PARAMS,
            &state.shared.dirty_params,
        );
        let (sdsp_scope_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 80.0),
            egui::Sense::hover(),
        );
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, sdsp_scope_rect, 48_000.0);

        // Read all band-frequency params once.
        let ess_gr_now = state.shared.ess_gr_db.load(Ordering::Relaxed);
        let tracked = state.shared.tracked_freq_hz.load(Ordering::Relaxed);
        let ess_range = state.shared.params[P_ESS_RANGE].load(Ordering::Relaxed);
        let lo_freq = state.shared.params[P_LO_FREQ].load(Ordering::Relaxed);
        let lo_amt = state.shared.params[P_LO_AMT].load(Ordering::Relaxed);
        let plos_on = state.shared.params[P_PLOS_ON].load(Ordering::Relaxed) >= 0.5;
        let plos_freq = state.shared.params[P_PLOS_FREQ].load(Ordering::Relaxed);
        let hum_on = state.shared.params[P_HUM_ON].load(Ordering::Relaxed) >= 0.5;
        let hum_freq = state.shared.params[P_HUM_FREQ].load(Ordering::Relaxed);

        // Colours — same hue family as the rest of the UI but distinct.
        let col_ess   = egui::Color32::from_rgb( 80, 230, 120);  // green   — sibilance cut
        let col_lo    = egui::Color32::from_rgb(120, 200, 230);  // cyan    — body cut
        let col_plos  = egui::Color32::from_rgb(230, 100,  90);  // red     — plosive HPF
        let col_hum   = egui::Color32::from_rgb(200, 130, 230);  // violet  — hum + harmonics

        // 1) Ess Range — translucent band ± range around the tracked freq.
        //    `Ess Range` is 0..1; map to ± 1.5 octaves max so the user
        //    can see exactly which slice of the spectrum the peak-EQ
        //    cuts when sibilance crosses the threshold.
        let span_oct = 1.5 * ess_range.max(0.05);
        let f_lo = (tracked / 2f32.powf(span_oct)).max(20.0);
        let f_hi = (tracked * 2f32.powf(span_oct)).min(20_000.0);
        let band_fill = egui::Color32::from_rgba_unmultiplied(
            col_ess.r(), col_ess.g(), col_ess.b(), 24,
        );
        core_gui::draw_spectrum_band_overlay(ui, sdsp_scope_rect, f_lo, f_hi, band_fill);

        // 2) Plosive HPF — cuts everything below `plos_freq`. Tint that zone.
        if plos_on {
            let plos_fill = egui::Color32::from_rgba_unmultiplied(
                col_plos.r(), col_plos.g(), col_plos.b(), 22,
            );
            core_gui::draw_spectrum_band_overlay(ui, sdsp_scope_rect, 20.0, plos_freq, plos_fill);
            core_gui::draw_spectrum_marker_colored(ui, sdsp_scope_rect, "Plos", plos_freq, 0.0, col_plos, false);
        }

        // 3) Hum fundamental + 5 harmonics.
        if hum_on {
            for h in 1..=6u32 {
                let f = hum_freq * h as f32;
                if f > 20_000.0 { break; }
                let label = if h == 1 { "Hum" } else { "" };
                core_gui::draw_spectrum_marker_colored(ui, sdsp_scope_rect, label, f, 0.0, col_hum, false);
            }
        }

        // 4) Lo band — peaking EQ centre. Show only when actually engaged.
        if lo_amt > 0.05 {
            core_gui::draw_spectrum_marker_colored(ui, sdsp_scope_rect, "Lo", lo_freq, -lo_amt, col_lo, true);
        }

        // 5) Ess tracker — always on top, with live GR readout.
        core_gui::draw_spectrum_marker_colored(ui, sdsp_scope_rect, "Ess", tracked, ess_gr_now, col_ess, true);

        // Stage meters: two GR bars side by side.
        let ess_gr = state.shared.ess_gr_db.load(Ordering::Relaxed);
        let click_gr = state.shared.click_gr_db.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("ess GR {ess_gr:>5.1} dB"))
                    .color(core_gui::GREEN_BRIGHT)
                    .monospace(),
            );
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new(format!("clk GR {click_gr:>5.1} dB"))
                    .color(core_gui::GREEN_BRIGHT)
                    .monospace(),
            );
            ui.add_space(20.0);
            // Listen as a top-level LED toggle — most-used during tuning.
            let g = core_gui::GestureBridge {
                begin: &state.shared.gesture_begin,
                end: &state.shared.gesture_end,
            };
            core_gui::dirty_toggle_row_g(
                ui,
                &state.shared.params[P_ESS_LISTEN],
                &PARAMS[P_ESS_LISTEN],
                &state.shared.dirty_params[P_ESS_LISTEN],
                g,
                P_ESS_LISTEN,
            );
            ui.add_space(12.0);
            core_gui::dirty_toggle_row_g(
                ui,
                &state.shared.params[P_SUB_MODE],
                &PARAMS[P_SUB_MODE],
                &state.shared.dirty_params[P_SUB_MODE],
                g,
                P_SUB_MODE,
            );
        });
        let sub_mode = state.shared.params[P_SUB_MODE].load(Ordering::Relaxed) >= 0.5;
        if sub_mode {
            ui.label(
                egui::RichText::new(
                    "SUB MODE: only the de-esser core runs. Plosive / Hum / De-Click / Lo are bypassed — \
                     use as the 2nd band when chained under a full Vocal instance.",
                )
                .color(core_gui::GREEN_DIM)
                .monospace()
                .small(),
            );
        }
        ui.add_space(4.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "De-Esser", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ESS_THR], &PARAMS[P_ESS_THR], &state.shared.dirty_params[P_ESS_THR], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ESS_THR);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ESS_FREQ], &PARAMS[P_ESS_FREQ], &state.shared.dirty_params[P_ESS_FREQ], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ESS_FREQ);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ESS_AMT], &PARAMS[P_ESS_AMT], &state.shared.dirty_params[P_ESS_AMT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ESS_AMT);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ESS_RANGE], &PARAMS[P_ESS_RANGE], &state.shared.dirty_params[P_ESS_RANGE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ESS_RANGE);
                core_gui::dirty_toggle_row_g(ui, &state.shared.params[P_ESS_TRACK], &PARAMS[P_ESS_TRACK], &state.shared.dirty_params[P_ESS_TRACK], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ESS_TRACK);
                let track_on = state.shared.params[P_ESS_TRACK].load(std::sync::atomic::Ordering::Relaxed) >= 0.5;
                let listen_on = state.shared.params[P_ESS_LISTEN].load(std::sync::atomic::Ordering::Relaxed) >= 0.5;
                let hint = match (track_on, listen_on) {
                    (true, true)   => "TRACK: cut follows the loudest sibilance band. LISTEN: monitoring removed signal.",
                    (true, false)  => "TRACK: HPF cutoff steers between 4.5-9 kHz based on energy ratio.",
                    (false, true)  => "LISTEN: output = sibilance × gain reduction only. Tune Thr & Amt by ear.",
                    (false, false) => "",
                };
                if !hint.is_empty() {
                    ui.label(egui::RichText::new(hint).color(core_gui::GREEN_DIM).monospace().small());
                }
            });
            core_gui::section(ui, "Low Band (plosives)", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LO_THR], &PARAMS[P_LO_THR], &state.shared.dirty_params[P_LO_THR], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LO_THR);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LO_FREQ], &PARAMS[P_LO_FREQ], &state.shared.dirty_params[P_LO_FREQ], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LO_FREQ);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LO_AMT], &PARAMS[P_LO_AMT], &state.shared.dirty_params[P_LO_AMT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LO_AMT);
            });
            core_gui::section(ui, "Sidechain", |ui| {
                core_gui::dirty_toggle_row_g(ui, &state.shared.params[P_EXT_KEY], &PARAMS[P_EXT_KEY], &state.shared.dirty_params[P_EXT_KEY], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_EXT_KEY);
            });
            core_gui::section(ui, "Plosive Killer (sub <250 Hz)", |ui| {
                core_gui::dirty_toggle_row_g(ui, &state.shared.params[P_PLOS_ON], &PARAMS[P_PLOS_ON], &state.shared.dirty_params[P_PLOS_ON], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_PLOS_ON);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_PLOS_THR], &PARAMS[P_PLOS_THR], &state.shared.dirty_params[P_PLOS_THR], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_PLOS_THR);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_PLOS_AMT], &PARAMS[P_PLOS_AMT], &state.shared.dirty_params[P_PLOS_AMT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_PLOS_AMT);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_PLOS_FREQ], &PARAMS[P_PLOS_FREQ], &state.shared.dirty_params[P_PLOS_FREQ], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_PLOS_FREQ);
            });
            core_gui::section(ui, "Hum Remover (50/60 Hz + 5 harmonics)", |ui| {
                core_gui::dirty_toggle_row_g(ui, &state.shared.params[P_HUM_ON], &PARAMS[P_HUM_ON], &state.shared.dirty_params[P_HUM_ON], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_HUM_ON);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_HUM_FREQ], &PARAMS[P_HUM_FREQ], &state.shared.dirty_params[P_HUM_FREQ], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_HUM_FREQ);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_HUM_STR], &PARAMS[P_HUM_STR], &state.shared.dirty_params[P_HUM_STR], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_HUM_STR);
            });
            core_gui::section(ui, "De-Clicker", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_CLK_SENS], &PARAMS[P_CLK_SENS], &state.shared.dirty_params[P_CLK_SENS], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_CLK_SENS);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_CLK_AMT], &PARAMS[P_CLK_AMT], &state.shared.dirty_params[P_CLK_AMT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_CLK_AMT);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_CLK_FLOOR], &PARAMS[P_CLK_FLOOR], &state.shared.dirty_params[P_CLK_FLOOR], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_CLK_FLOOR);
            });
            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_OUTPUT);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MIX], &PARAMS[P_MIX], &state.shared.dirty_params[P_MIX], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_MIX);
            });

            core_gui::help_block(
                ui,
                "vocal_help",
                &[
                    (
                        "Workflow",
                        "1. Engage `Listen` to monitor only the sibilance the de-esser \
                         cuts. 2. Set Ess Freq to the centre of the harshness (4-9 kHz). \
                         3. Lower Ess Thr until the GR meter ducks 4-8 dB on loud `s`. \
                         4. Turn Listen off — the cut is in place. Ess Range narrows the \
                         peaking-EQ Q (1.0 = wide, 0.3 = surgical).",
                    ),
                    (
                        "Ess Track",
                        "Frequency-tracking mode (Sibalance-style). Sets Ess Freq to \
                         follow the loudest sub-band between 4.5 kHz (\"s\") and 9 kHz \
                         (\"sh\") in real time. Use for material where sibilance moves \
                         around. The green pointer on the spectrum shows where the cut \
                         is right now.",
                    ),
                    (
                        "Sub Mode",
                        "When ON, only the de-esser core runs — Plosive Killer, Hum \
                         Remover, De-Click and Lo body cut are bypassed. Use as the 2nd \
                         instance in a 2-band chain (Sib 1 / Sib 2 presets) so the shared \
                         cleanup stages don't run twice.",
                    ),
                    (
                        "Spectrum markers",
                        "Green = de-esser cut (tracked freq + Range band). Cyan = Lo \
                         body cut. Red = Plosive HPF zone. Violet = Hum fundamental + 5 \
                         harmonics. Markers only appear when the corresponding stage is \
                         engaged.",
                    ),
                    (
                        "Ext Key",
                        "When ON, the de-esser detector listens to the sidechain input \
                         (port 2). Route a comp-side trigger track via REAPER's pin \
                         connector. Off = self-keyed off the dry signal.",
                    ),
                ],
            );
        });
    });
}
