//! Shared GUI infrastructure for SuperDuper effect plugins.
//!
//! Every effect plugin needs the same skeleton:
//!   - host-driven resize bridge (Arc<(AtomicU32, AtomicU32)>)
//!   - egui style (font sizes, slider width, item spacing)
//!   - "section" header rendering
//!   - parameter row (label + slider + unit)
//!   - preset dropdown
//!
//! Pulling this into one place means a new effect's `gui.rs` only declares
//! per-plugin specifics (title, default size, parameter layout, preset list)
//! and calls into these helpers.
//!
//! Enabled via the `gui` feature so test-only consumers of `synth-core`
//! don't pull in `egui`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use atomic_float::AtomicF32;
use superduper_dsp_sdk::clap_helpers::ParamDef;

/// Same as [`param_row`] but also raises a dirty flag whenever the user
/// changes the value, so the audio thread can emit a CLAP `ParamValue`
/// event into the host's output queue. That tells the DAW (REAPER /
/// Bitwig / etc) to record the move into its automation lane —
/// without this, GUI knob moves are invisible to the host.
pub fn dirty_param_row(
    ui: &mut egui::Ui,
    atom: &AtomicF32,
    def: &ParamDef,
    dirty: &AtomicBool,
) {
    let before = atom.load(Ordering::Relaxed);
    param_row(ui, atom, def);
    let after = atom.load(Ordering::Relaxed);
    if (after - before).abs() > 1e-9 {
        dirty.store(true, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Resize bridge — main thread (CLAP `set_size`) → GUI thread (queue.resize)
// ---------------------------------------------------------------------------

pub type ResizeBridge = Arc<(AtomicU32, AtomicU32)>;

pub fn new_resize_bridge(default_w: u32, default_h: u32) -> ResizeBridge {
    Arc::new((AtomicU32::new(default_w), AtomicU32::new(default_h)))
}

pub fn read_bridge(bridge: &ResizeBridge) -> (u32, u32) {
    (
        bridge.0.load(Ordering::Relaxed),
        bridge.1.load(Ordering::Relaxed),
    )
}

pub fn write_bridge(bridge: &ResizeBridge, w: u32, h: u32) {
    bridge.0.store(w, Ordering::Relaxed);
    bridge.1.store(h, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

// ===========================================================================
// Theme — retro phosphor-CRT / hacker terminal: pure black background,
// bright green text/lines, monospace font, tight spacing. Inspired by old
// Tek scopes and the original VST plugins that came on Atari ST.
// ===========================================================================

// Phosphor green calibrated to look like a Tek scope or vintage VT220 — NOT
// pure-bright neon. Saturated but with enough yellow-warm to be readable
// without burning the eyes.
pub const BG: egui::Color32 = egui::Color32::from_rgb(10, 14, 12);
pub const PANEL_BG: egui::Color32 = egui::Color32::from_rgb(16, 22, 18);
pub const TRACK_BG: egui::Color32 = egui::Color32::from_rgb(34, 44, 38);
pub const GREEN_BRIGHT: egui::Color32 = egui::Color32::from_rgb(120, 220, 140);
pub const GREEN: egui::Color32 = egui::Color32::from_rgb(90, 180, 110);
pub const GREEN_DIM: egui::Color32 = egui::Color32::from_rgb(60, 130, 80);
pub const GREEN_FAINT: egui::Color32 = egui::Color32::from_rgb(36, 80, 50);
/// Bright accent — same as GREEN_BRIGHT but the name is referenced from old
/// per-plugin code.
pub const SECTION_COLOR: egui::Color32 = GREEN_BRIGHT;

/// Apply SuperDuper's shared egui theme. Compact (12 px body), monospace,
/// green-on-black "old terminal" look. One-shot — call from the
/// egui-baseview `build` closure.
pub fn install_default_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    use egui::{FontFamily::Monospace, FontId, TextStyle};
    style.text_styles = [
        (TextStyle::Heading, FontId::new(15.0, Monospace)),
        (TextStyle::Body, FontId::new(12.0, Monospace)),
        (TextStyle::Button, FontId::new(12.0, Monospace)),
        (TextStyle::Small, FontId::new(10.0, Monospace)),
        (TextStyle::Monospace, FontId::new(12.0, Monospace)),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.slider_width = 200.0;
    style.spacing.button_padding = egui::vec2(6.0, 3.0);

    let v = &mut style.visuals;
    v.dark_mode = true;
    v.override_text_color = Some(GREEN);
    v.window_fill = BG;
    v.panel_fill = BG;
    v.extreme_bg_color = PANEL_BG;
    v.faint_bg_color = PANEL_BG;
    v.code_bg_color = PANEL_BG;
    v.window_stroke = egui::Stroke::new(1.0, GREEN_DIM);

    let widgets = &mut v.widgets;

    // `noninteractive` colours backgrounds of labels, panels, separators.
    widgets.noninteractive.bg_fill = PANEL_BG;
    widgets.noninteractive.weak_bg_fill = PANEL_BG;
    widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, GREEN_FAINT);
    widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, GREEN);

    // `inactive` = a slider/button/combobox at rest. The slider TRACK uses
    // weak_bg_fill, the THUMB and outline use bg_fill + bg_stroke. We make
    // both visible against the dark panel.
    widgets.inactive.bg_fill = GREEN_DIM;            // thumb / button face
    widgets.inactive.weak_bg_fill = TRACK_BG;        // slider track
    widgets.inactive.bg_stroke = egui::Stroke::new(1.0, GREEN);
    widgets.inactive.fg_stroke = egui::Stroke::new(1.0, GREEN_BRIGHT);

    widgets.hovered.bg_fill = GREEN;
    widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(44, 60, 50);
    widgets.hovered.bg_stroke = egui::Stroke::new(1.5, GREEN_BRIGHT);
    widgets.hovered.fg_stroke = egui::Stroke::new(1.5, GREEN_BRIGHT);

    widgets.active.bg_fill = GREEN_BRIGHT;
    widgets.active.weak_bg_fill = egui::Color32::from_rgb(60, 80, 70);
    widgets.active.bg_stroke = egui::Stroke::new(1.5, GREEN_BRIGHT);
    widgets.active.fg_stroke = egui::Stroke::new(1.5, GREEN_BRIGHT);

    widgets.open.bg_fill = GREEN_DIM;
    widgets.open.weak_bg_fill = TRACK_BG;
    widgets.open.bg_stroke = egui::Stroke::new(1.0, GREEN_BRIGHT);
    widgets.open.fg_stroke = egui::Stroke::new(1.0, GREEN_BRIGHT);

    v.selection.bg_fill = egui::Color32::from_rgb(40, 80, 56);
    v.selection.stroke = egui::Stroke::new(1.0, GREEN_BRIGHT);
    v.hyperlink_color = GREEN_BRIGHT;

    ctx.set_style(style);
}

// ---------------------------------------------------------------------------
// Layout helpers
// ---------------------------------------------------------------------------

/// Render an ASCII-style section header followed by the body closure.
/// Looks like `== TITLE =====================`, very-old-terminal-flavored.
pub fn section<R>(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.add_space(4.0);
    // Build a fixed-width header: "== TITLE " padded with '=' out to ~60 cols.
    let upper = title.to_uppercase();
    let pad_len = 56_usize.saturating_sub(upper.len() + 4);
    let header = format!("══ {upper} {}", "═".repeat(pad_len));
    ui.label(
        egui::RichText::new(header)
            .color(SECTION_COLOR)
            .monospace(),
    );
    let r = body(ui);
    ui.add_space(2.0);
    r
}

/// Render one parameter row: monospace label (90 px) + slider + value + unit.
/// Reads `atom` atomically, writes back on change.
pub fn param_row(ui: &mut egui::Ui, atom: &AtomicF32, def: &ParamDef) {
    let mut value = atom.load(Ordering::Relaxed);
    let name = std::str::from_utf8(def.name)
        .unwrap_or("?")
        .trim_end_matches('\0');

    ui.horizontal(|ui| {
        ui.add_sized(
            [90.0, 18.0],
            egui::Label::new(egui::RichText::new(name).color(GREEN).monospace()),
        );
        let slider = egui::Slider::new(&mut value, (def.min as f32)..=(def.max as f32))
            .show_value(true)
            .clamping(egui::SliderClamping::Always)
            .suffix(if def.unit.is_empty() {
                String::new()
            } else {
                format!(" {}", def.unit)
            });
        if ui.add(slider).changed() {
            atom.store(value, Ordering::Relaxed);
        }
    });
}

/// Preset selector dropdown. Updates `selected` if user picks a new preset
/// and returns `true` on change.
pub fn preset_combo(
    ui: &mut egui::Ui,
    combo_id: &str,
    preset_names: &[&'static str],
    selected: &mut Option<usize>,
) -> bool {
    let mut clicked: Option<usize> = None;
    let current_name = selected.and_then(|i| preset_names.get(i).copied()).unwrap_or("—");
    egui::ComboBox::from_id_salt(combo_id)
        .selected_text(current_name)
        .width(180.0)
        .show_ui(ui, |ui| {
            for (i, name) in preset_names.iter().enumerate() {
                if ui
                    .selectable_label(*selected == Some(i), *name)
                    .clicked()
                {
                    clicked = Some(i);
                }
            }
        });
    if let Some(i) = clicked {
        *selected = Some(i);
        true
    } else {
        false
    }
}

/// Title bar: `sdsp> TITLE  v0.X.NNNNN  ──────  [preset] [bypass]`.
/// Returns the newly selected preset index, if any.
pub fn top_bar(
    ui: &mut egui::Ui,
    title: &str,
    build_num: &str,
    build_date: &str,
    bypass: &AtomicBool,
    combo_id: &str,
    preset_names: &[&'static str],
    selected_preset: &mut Option<usize>,
) -> Option<usize> {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!("sdsp> {}", title.to_uppercase()))
                .color(GREEN_BRIGHT)
                .monospace()
                .strong(),
        );
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(format!("b{build_num} {build_date}"))
                .color(GREEN_DIM)
                .monospace(),
        );
    });
    let mut new_selection = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("preset:").color(GREEN).monospace());
        if preset_combo(ui, combo_id, preset_names, selected_preset) {
            new_selection = *selected_preset;
        }
        ui.add_space(12.0);
        let mut bypassed = bypass.load(Ordering::Relaxed);
        let label = if bypassed { "[X] bypass" } else { "[ ] bypass" };
        if ui
            .selectable_label(bypassed, egui::RichText::new(label).color(GREEN).monospace())
            .clicked()
        {
            bypassed = !bypassed;
            bypass.store(bypassed, Ordering::Relaxed);
        }
    });
    // Full-width separator line in green dim.
    let rect = ui.available_rect_before_wrap();
    let y = rect.top() + 2.0;
    ui.painter().line_segment(
        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
        egui::Stroke::new(1.0, GREEN_DIM),
    );
    ui.add_space(8.0);
    new_selection
}
