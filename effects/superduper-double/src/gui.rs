//! egui_baseview GUI for SuperDuper Double.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering::Relaxed;
use superduper_synth_core::gui as core_gui;
use superduper_synth_core::scale;

use crate::presets::PRESETS;
use crate::{
    SharedParams, MAX_VOICES, MODE_NAMES, PARAMS, P_DRY, P_FORMANT, P_KEY, P_KEY_OPEN,
    P_KEY_RANGE, P_LEVEL, P_MIX, P_MODE, P_OUTPUT, P_PITCH_VAR, P_RETUNE, P_SCALE, P_SEED,
    P_SPREAD, P_TIMING, P_VOICES,
};

pub const DEFAULT_WIDTH: u32 = 480;
pub const DEFAULT_HEIGHT: u32 = 560;
pub const MIN_WIDTH: u32 = 380;
pub const MIN_HEIGHT: u32 = 380;
pub const MAX_WIDTH: u32 = 1200;
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
    scale_names: Vec<&'static str>,
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P,
    shared: SharedParams,
    resize: ResizeBridge,
) -> WindowHandle {
    let (w, h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Double".to_string(),
        size: Size::new(w as f64, h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let preset = (shared.active_preset.load(Relaxed) as usize).min(PRESETS.len().saturating_sub(1));
    let state = GuiState {
        shared,
        resize,
        applied_size: (w, h),
        selected_preset: Some(preset),
        preset_names: PRESETS.iter().map(|p| p.name).collect(),
        scale_names: scale::SCALES.iter().map(|s| s.0).collect(),
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
        if let Some(i) = core_gui::top_bar_mix(
            &state.shared.scope,
            ui,
            "SuperDuper Double",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "double_preset_combo",
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

        draw_voices(ui, &state.shared);
        ui.add_space(6.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Doubles", |ui| {
                choice(ui, state, P_MODE, &MODE_NAMES);
                param(ui, state, P_VOICES);
                param(ui, state, P_TIMING);
                param(ui, state, P_PITCH_VAR);
                param(ui, state, P_FORMANT);
                param(ui, state, P_SEED);
            });
            let names = state.scale_names.clone();
            core_gui::section(ui, "Tuning (Tuned / Tuned Lead)", |ui| {
                choice(ui, state, P_KEY, &scale::KEY_NAMES);
                choice(ui, state, P_SCALE, &names);
                param(ui, state, P_RETUNE);
            });
            core_gui::section(ui, "Output", |ui| {
                param(ui, state, P_SPREAD);
                param(ui, state, P_LEVEL);
                param(ui, state, P_MIX);
                param(ui, state, P_DRY);
                param(ui, state, P_OUTPUT);
            });
            core_gui::section(ui, "Key (doubles swell with another track)", |ui| {
                superduper_synth_core::mix_view::key_picker(ui, &state.shared.scope);
                param(ui, state, P_KEY_OPEN);
                param(ui, state, P_KEY_RANGE);
            });
            help(ui);
        });
    });
}

/// One row per voice: where it sits in time (0..Timing) and in pitch (±var).
fn draw_voices(ui: &mut egui::Ui, sh: &crate::SharedParamsInner) {
    let timing = sh.params[P_TIMING].load(Relaxed).max(1.0);
    let var = sh.params[P_PITCH_VAR].load(Relaxed).max(5.0);
    let hz = sh.detected_hz.load(Relaxed);
    let lead = sh.lead_corr_st.load(Relaxed) * 100.0;
    ui.label(
        egui::RichText::new(format!(
            "in {:>6.1} Hz   lead {:+5.1} ct   key gain {:.2}",
            hz,
            lead,
            sh.key_gain.load(Relaxed)
        ))
        .color(core_gui::GREEN_DIM),
    );
    let row_h = 18.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_h * MAX_VOICES as f32 + 4.0),
        egui::Sense::hover(),
    );
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, core_gui::PANEL_BG);
    let label_w = 30.0;
    let text_w = 120.0;
    let lane_w = ((rect.width() - label_w - text_w - 12.0) * 0.5).max(20.0);
    let font = egui::FontId::monospace(11.0);
    for v in 0..MAX_VOICES {
        let y = rect.top() + 2.0 + row_h * v as f32;
        let d = sh.voice_delay_ms[v].load(Relaxed);
        let c = sh.voice_cents[v].load(Relaxed);
        let on = d >= 0.0;
        let col = if on { core_gui::GREEN_BRIGHT } else { core_gui::GREEN_FAINT };
        let mid = y + row_h * 0.5;
        p.text(egui::pos2(rect.left() + 6.0, mid), egui::Align2::LEFT_CENTER, format!("V{}", v + 1), font.clone(), col);
        // Time lane: lead at the left edge, Timing at the right.
        let t_lane = egui::Rect::from_min_size(egui::pos2(rect.left() + label_w, y + 4.0), egui::vec2(lane_w, row_h - 8.0));
        // Pitch lane: centre = in tune with the lead.
        let p_lane = egui::Rect::from_min_size(egui::pos2(t_lane.right() + 6.0, y + 4.0), egui::vec2(lane_w, row_h - 8.0));
        for lane in [t_lane, p_lane] {
            p.rect_filled(lane, 2.0, core_gui::TRACK_BG);
        }
        p.line_segment([egui::pos2(p_lane.center().x, p_lane.top()), egui::pos2(p_lane.center().x, p_lane.bottom())], egui::Stroke::new(1.0, core_gui::GREEN_FAINT));
        if on {
            let tx = t_lane.left() + t_lane.width() * (d / timing).clamp(0.0, 1.0);
            let px = p_lane.center().x + p_lane.width() * 0.5 * (c / var).clamp(-1.0, 1.0);
            p.circle_filled(egui::pos2(tx, t_lane.center().y), 4.0, core_gui::GREEN_BRIGHT);
            p.circle_filled(egui::pos2(px, p_lane.center().y), 4.0, core_gui::GREEN);
            p.text(
                egui::pos2(rect.right() - 6.0, mid),
                egui::Align2::RIGHT_CENTER,
                format!("{:5.1} ms {:+6.1} ct", d, c),
                font.clone(),
                col,
            );
        } else {
            p.text(egui::pos2(rect.right() - 6.0, mid), egui::Align2::RIGHT_CENTER, "off", font.clone(), col);
        }
    }
}

fn help(ui: &mut egui::Ui) {
    core_gui::help_block(
        ui,
        "double_help",
        &[
            (
                "What a double is",
                "Each voice is the same line 're-sung': it lags the lead by 0.3–1 × Timing and \
                 wanders ± Pitch Var cents, both slowly (0.3–1 Hz) and independently per voice. \
                 Timing is moved by choosing grains, not by a moving delay line, so it never \
                 bends the pitch (a 30 ms sweep in a delay line is a ±50 ct chorus). Formants \
                 stay put; Formant nudges them for a different singer.",
            ),
            (
                "Modes",
                "Natural = human doubles. Tuned = the doubles snap to Key + Scale at the Retune \
                 speed (Pitch Var is off — they are the tuned reference) while the lead stays \
                 natural. Tuned Lead = the reverse: the lead is snapped, the doubles stay human.",
            ),
            (
                "Seed, Spread, Level",
                "Seed picks each voice's character (its rates and a 0 to −2.5 dB level offset); \
                 change it if a take beats against the lead. Spread fans the voices from the \
                 centre to hard L/R. Dry 0 prints the doubles alone.",
            ),
            (
                "Key Open",
                "Pick a key track (master or drums). At Key Open 1 the doubles are silent while \
                 the key is quiet and full once it is Key Range dB above −50 dB — doubles that \
                 bloom in the chorus. 0 = off.",
            ),
            (
                "Latency",
                "Reports the PSOLA look-behind (57 ms at 48 kHz, as Pitch and Tune) so the host \
                 compensates. Mix 0 is the dry signal, bit for bit, that much late.",
            ),
        ],
    );
}

fn param(ui: &mut egui::Ui, state: &GuiState, idx: usize) {
    core_gui::dirty_param_row_g(
        ui,
        &state.shared.params[idx],
        &PARAMS[idx],
        &state.shared.dirty_params[idx],
        core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end },
        idx,
    );
}

fn choice(ui: &mut egui::Ui, state: &GuiState, idx: usize, options: &[&str]) {
    core_gui::dirty_choice_row_g(
        ui,
        &state.shared.params[idx],
        &PARAMS[idx],
        options,
        &state.shared.dirty_params[idx],
        core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end },
        idx,
    );
}

fn apply_preset(state: &mut GuiState, index: usize) {
    if let Some(preset) = PRESETS.get(index) {
        crate::presets::apply(&state.shared, preset);
        state.shared.active_preset.store(index as u32, Relaxed);
    }
}
