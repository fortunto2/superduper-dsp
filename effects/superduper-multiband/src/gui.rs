//! GUI for SuperDuper Multiband.
//!
//! Top to bottom: top bar (preset, bypass, mix strip) → spectrum with the
//! bands, their live gain reduction and draggable crossovers
//! (`gui_spectrum`) → band tabs with Solo / Bypass → the selected band's
//! dynamics → the bus key → global (band count, crossovers, mix, output).

use std::sync::atomic::{AtomicBool, Ordering};

use atomic_float::AtomicF32;
use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use superduper_synth_core::gui as core_gui;

use crate::dsp::{band_range, MAX_BANDS};
use crate::presets::PRESETS;
use crate::{
    bp, settings_from, SharedParams, F_ATTACK, F_BYPASS, F_KEY, F_KNEE, F_MAKEUP, F_RANGE, F_RATIO,
    F_RELEASE, F_SOLO, F_THRESHOLD, P_BANDS, P_MIX, P_OUTPUT, P_XOVER, PARAMS,
};

pub const DEFAULT_WIDTH: u32 = 780;
pub const DEFAULT_HEIGHT: u32 = 720;
pub const MIN_WIDTH: u32 = 560;
pub const MIN_HEIGHT: u32 = 420;
pub const MAX_WIDTH: u32 = 1800;
pub const MAX_HEIGHT: u32 = 1400;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge { core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT) }

/// One colour per band, shared by the spectrum overlay and the tabs.
pub const BAND_COLORS: [egui::Color32; MAX_BANDS] = [
    egui::Color32::from_rgb(240, 178, 92),  // amber
    egui::Color32::from_rgb(96, 200, 230),  // cyan
    egui::Color32::from_rgb(240, 96, 210),  // magenta
    egui::Color32::from_rgb(150, 230, 110), // lime
];

pub(crate) struct GuiState {
    pub shared: SharedParams,
    resize: ResizeBridge,
    applied_size: (u32, u32),
    selected_preset: Option<usize>,
    preset_names: Vec<&'static str>,
    pub selected_band: usize,
    /// Crossover being dragged on the spectrum, if any.
    pub drag: Option<usize>,
}

pub fn open_window<P: HasRawWindowHandle>(parent: &P, shared: SharedParams, resize: ResizeBridge) -> WindowHandle {
    let (w, h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Multiband".to_string(),
        size: Size::new(w as f64, h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let preset = (shared.active_preset.load(Ordering::Relaxed) as usize).min(PRESETS.len().saturating_sub(1));
    let state = GuiState {
        shared,
        resize,
        applied_size: (w, h),
        selected_preset: Some(preset),
        preset_names: PRESETS.iter().map(|p| p.name).collect(),
        selected_band: 0,
        drag: None,
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

/// Slider row for param `i` with dirty + gesture plumbing.
pub(crate) fn row(ui: &mut egui::Ui, sh: &crate::SharedParamsInner, i: usize) {
    let g = core_gui::GestureBridge { begin: &sh.gesture_begin, end: &sh.gesture_end };
    core_gui::dirty_param_row_g(ui, &sh.params[i], &PARAMS[i], &sh.dirty_params[i], g, i);
}

/// Store a param from the GUI the way the host must hear it: value, dirty,
/// and a begin/end gesture pulse.
pub(crate) fn set_param(sh: &crate::SharedParamsInner, i: usize, v: f32) {
    sh.params[i].store(v, Ordering::Relaxed);
    sh.dirty_params[i].store(true, Ordering::Relaxed);
}

fn pulse_gesture(sh: &crate::SharedParamsInner, i: usize) {
    sh.gesture_begin[i].store(true, Ordering::Relaxed);
    sh.gesture_end[i].store(true, Ordering::Relaxed);
}

fn draw(ctx: &egui::Context, state: &mut GuiState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        if let Some(i) = core_gui::top_bar_mix(
            &state.shared.scope,
            ui,
            "SuperDuper Multiband",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "mb_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            if let Some(p) = PRESETS.get(i) {
                crate::presets::apply(&state.shared, p);
                state.shared.active_preset.store(i as u32, Ordering::Relaxed);
            }
        }
        core_gui::ab_init_bar(ui, &state.shared.ab_snapshot, &state.shared.params, PARAMS, &state.shared.dirty_params);

        crate::gui_spectrum::draw(ui, state);
        ui.add_space(4.0);
        band_tabs(ui, state);
        ui.add_space(2.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            band_section(ui, state);
            core_gui::section(ui, "Key (bus track — one per instance)", |ui| {
                superduper_synth_core::mix_view::key_picker(ui, &state.shared.scope);
                ui.label(
                    egui::RichText::new("Each band hears the key only in its own range. B<n> Key: 0 % = own signal, 100 % = the key alone.")
                        .color(core_gui::GREEN_DIM)
                        .small(),
                );
            });
            global_section(ui, &state.shared);
            help(ui);
        });
    });
}

fn band_tabs(ui: &mut egui::Ui, state: &mut GuiState) {
    let sh = &state.shared;
    let s = settings_from(&sh.params);
    if state.selected_band >= s.bands { state.selected_band = s.bands - 1; }
    ui.horizontal(|ui| {
        for b in 0..s.bands {
            let gr = sh.meters.gr_db[b].load(Ordering::Relaxed);
            let (lo, hi) = band_range(s.bands, &s.xover_hz, b);
            let text = egui::RichText::new(format!("B{} {}–{}  {:+.1} dB", b + 1, hz(lo), hz(hi), gr))
                .color(BAND_COLORS[b])
                .monospace();
            if ui.selectable_label(state.selected_band == b, text).clicked() { state.selected_band = b; }
        }
    });
    let b = state.selected_band;
    ui.horizontal(|ui| {
        for (field, label) in [(F_SOLO, "solo"), (F_BYPASS, "bypass")] {
            let i = bp(b, field);
            let on = sh.params[i].load(Ordering::Relaxed) >= 0.5;
            let txt = format!("[{}] B{} {label}", if on { "X" } else { " " }, b + 1);
            if ui.selectable_label(on, egui::RichText::new(txt).color(core_gui::GREEN).monospace()).clicked() {
                set_param(sh, i, if on { 0.0 } else { 1.0 });
                pulse_gesture(sh, i);
            }
        }
    });
}

fn band_section(ui: &mut egui::Ui, state: &GuiState) {
    let sh = &state.shared;
    let b = state.selected_band;
    core_gui::section(ui, &format!("Band {}", b + 1), |ui| {
        for f in [F_THRESHOLD, F_RATIO, F_KNEE, F_ATTACK, F_RELEASE, F_RANGE, F_MAKEUP, F_KEY] {
            row(ui, sh, bp(b, f));
        }
    });
}

fn global_section(ui: &mut egui::Ui, sh: &crate::SharedParamsInner) {
    core_gui::section(ui, "Global", |ui| {
        ui.horizontal(|ui| {
            ui.add_sized([120.0, 18.0], egui::Label::new(egui::RichText::new("Bands").color(core_gui::GREEN).monospace()));
            let cur = sh.params[P_BANDS].load(Ordering::Relaxed).round() as usize;
            for n in 2..=MAX_BANDS {
                let txt = format!("{}{n}", if cur == n { "●" } else { "○" });
                if ui.selectable_label(cur == n, egui::RichText::new(txt).color(core_gui::GREEN).monospace()).clicked() {
                    set_param(sh, P_BANDS, n as f32);
                    pulse_gesture(sh, P_BANDS);
                }
            }
        });
        let bands = sh.params[P_BANDS].load(Ordering::Relaxed).round() as usize;
        for (c, &i) in P_XOVER.iter().enumerate().take(bands.saturating_sub(1)) {
            log_hz_row(ui, &sh.params[i], &sh.dirty_params[i], &sh.gesture_begin[i], &sh.gesture_end[i], c);
        }
        row(ui, sh, P_MIX);
        row(ui, sh, P_OUTPUT);
    });
}

/// Crossover slider on a log axis (a linear 20 Hz – 20 kHz slider puts
/// everything below 1 kHz in the first 5 %).
fn log_hz_row(ui: &mut egui::Ui, atom: &AtomicF32, dirty: &AtomicBool, begin: &AtomicBool, end: &AtomicBool, c: usize) {
    ui.horizontal(|ui| {
        ui.add_sized([120.0, 18.0], egui::Label::new(egui::RichText::new(format!("Xover {}", c + 1)).color(core_gui::GREEN).monospace()));
        let mut v = atom.load(Ordering::Relaxed);
        let r = ui.add(egui::Slider::new(&mut v, 20.0..=20_000.0).logarithmic(true).suffix(" Hz").max_decimals(0));
        if r.drag_started() { begin.store(true, Ordering::Relaxed); }
        if r.changed() {
            atom.store(v, Ordering::Relaxed);
            dirty.store(true, Ordering::Relaxed);
        }
        if r.drag_stopped() { end.store(true, Ordering::Relaxed); }
    });
}

pub(crate) fn hz(f: f32) -> String {
    if f >= 1000.0 { format!("{:.1}k", f / 1000.0) } else { format!("{f:.0}") }
}

fn help(ui: &mut egui::Ui) {
    core_gui::help_block(
        ui,
        "mb_help",
        &[
            (
                "Dynamic phase",
                "The audio never passes through the crossovers. Band gains are applied as \
                 shelves at the crossover points, and a shelf with nothing to do is skipped — \
                 so with no band compressing the output is the input, bit for bit. A band \
                 that compresses adds an analog-style phase shift, only while it works.",
            ),
            (
                "Bands",
                "Detection uses Linkwitz-Riley 24 dB/oct crossovers, so each band reacts to \
                 what is really in it. Drag the vertical lines on the spectrum to move the \
                 crossovers; click a band to edit it. Solo listens to a band through the \
                 crossover.",
            ),
            (
                "Bus key",
                "Pick another track on the SuperDuper bus. Its level inside each band's \
                 range is what that band's Key amount listens to: the 250 Hz–4 kHz band \
                 ducks under the vocal's body while the lows keep compressing the bass. \
                 Key levels are band RMS on the bus scale, −50 dB is 'loud'.",
            ),
            (
                "Ratio below 1:1",
                "Upward expansion: the band gets louder above threshold, capped by Range \
                 (or +12 dB when Range is 0).",
            ),
        ],
    );
}
