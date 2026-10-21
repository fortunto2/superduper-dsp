use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::targets::KUBYZ_NAMES;
use crate::{
    SharedParams, MODE_NAMES, PARAMS, P_BREATH, P_MIX, P_MODE, P_MORPH, P_OUTPUT, P_SOURCE,
    P_TARGET, P_TILT, P_VOWELS, SOURCE_NAMES,
};

pub const DEFAULT_WIDTH: u32 = 640;
pub const DEFAULT_HEIGHT: u32 = 620;
pub const MIN_WIDTH: u32 = 480;
pub const MIN_HEIGHT: u32 = 420;
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

pub fn open_window<P: HasRawWindowHandle>(parent: &P, shared: SharedParams, resize: ResizeBridge) -> WindowHandle {
    let (w, h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Morph".to_string(),
        size: Size::new(w as f64, h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let preset = (shared.active_preset.load(Ordering::Relaxed) as usize).min(PRESETS.len() - 1);
    let state = GuiState {
        shared,
        resize,
        applied_size: (w, h),
        selected_preset: Some(preset),
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

/// The big Morph slider: the one control that matters, full width.
fn morph_slider(ui: &mut egui::Ui, s: &SharedParams) {
    let mut v = s.params[P_MORPH].load(Ordering::Relaxed);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("MORPH").monospace().strong().size(18.0));
        ui.spacing_mut().slider_width = (ui.available_width() - 80.0).max(120.0);
        let r = ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(true).fixed_decimals(2));
        if r.drag_started() {
            s.gesture_begin[P_MORPH].store(true, Ordering::Relaxed);
        }
        if r.changed() {
            s.write_param(P_MORPH, v);
        }
        if r.drag_stopped() {
            s.gesture_end[P_MORPH].store(true, Ordering::Relaxed);
        }
    });
    ui.label(egui::RichText::new("voice  ←────────────────→  target").monospace().weak());
}

/// Voice / target / output harmonic levels at the current f0, as bars.
fn harmonic_display(ui: &mut egui::Ui, s: &SharedParams) {
    let l = &s.live;
    let n = (l.n.load(Ordering::Relaxed) as usize).min(24);
    let mode = ["pass", "harmonic", "envelope"][(l.mode.load(Ordering::Relaxed) as usize).min(2)];
    let sc = if l.sc_level_db.load(Ordering::Relaxed) > -60.0 {
        format!("target in {:.0} Hz{}", l.sc_f0.load(Ordering::Relaxed), if l.sc_voiced.load(Ordering::Relaxed) { "" } else { " (unvoiced)" })
    } else {
        "target in: silent".to_string()
    };
    ui.label(egui::RichText::new(format!(
        "f0 {:.1} Hz {} · frame: {mode} · {sc}",
        l.f0.load(Ordering::Relaxed),
        if l.voiced.load(Ordering::Relaxed) { "voiced" } else { "unvoiced" },
    )).monospace());
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 110.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, egui::Color32::from_gray(18));
    if n == 0 {
        return;
    }
    let colours = [
        egui::Color32::from_rgb(90, 160, 90),   // voice
        egui::Color32::from_rgb(230, 150, 60),  // target
        egui::Color32::from_rgb(180, 240, 180), // output
    ];
    let slot = rect.width() / n as f32;
    let bw = (slot / 4.0).max(1.0);
    for k in 0..n {
        let vals = [&l.voice[k], &l.target[k], &l.out[k]];
        for (j, a) in vals.iter().enumerate() {
            let db = a.load(Ordering::Relaxed).clamp(-60.0, 0.0);
            let h = (db + 60.0) / 60.0 * rect.height();
            let x = rect.left() + k as f32 * slot + bw * (j as f32 + 0.5);
            p.rect_filled(egui::Rect::from_min_max(egui::pos2(x, rect.bottom() - h), egui::pos2(x + bw, rect.bottom())), 0.0, colours[j]);
        }
    }
    ui.label(egui::RichText::new("bars per harmonic: voice · target · output (dB, energy-normalised)").monospace().weak());
}

fn capture_row(ui: &mut egui::Ui, s: &SharedParams) {
    ui.horizontal(|ui| {
        if ui.button("Capture").on_hover_text("Freeze the timbre on the Target input into the project").clicked() {
            s.capture_request.store(true, Ordering::Release);
        }
        let c = s.captured.load();
        let txt = if !c.env_valid {
            "nothing captured".to_string()
        } else if c.harm_valid {
            format!("captured: pitched, {:.0} Hz, noise {:.0}%", c.f0, c.noise * 100.0)
        } else {
            "captured: envelope only (unpitched)".to_string()
        };
        ui.label(egui::RichText::new(txt).monospace());
    });
}

fn draw(ctx: &egui::Context, state: &mut GuiState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let s = state.shared.clone();
        if let Some(i) = core_gui::top_bar_mix(
            &s.scope, ui, "SuperDuper Morph", env!("SDSP_BUILD_NUM"), env!("SDSP_BUILD_DATE"),
            &s.bypass, "morph_preset_combo", &state.preset_names, &mut state.selected_preset,
        ) {
            if let Some(p) = PRESETS.get(i) {
                crate::presets::apply(&s, p);
                s.active_preset.store(i as u32, Ordering::Relaxed);
            }
        }
        core_gui::ab_init_bar(ui, &s.ab_snapshot, &s.params, PARAMS, &s.dirty_params);
        ui.add_space(6.0);
        morph_slider(ui, &s);
        ui.add_space(4.0);
        harmonic_display(ui, &s);
        egui::ScrollArea::vertical().show(ui, |ui| {
            let g = || core_gui::GestureBridge { begin: &s.gesture_begin, end: &s.gesture_end };
            let row = |ui: &mut egui::Ui, i: usize| core_gui::dirty_param_row_g(ui, &s.params[i], &PARAMS[i], &s.dirty_params[i], g(), i);
            let choice = |ui: &mut egui::Ui, i: usize, names: &[&str]| {
                core_gui::dirty_choice_row_g(ui, &s.params[i], &PARAMS[i], names, &s.dirty_params[i], g(), i)
            };
            core_gui::section(ui, "Target", |ui| {
                choice(ui, P_SOURCE, &SOURCE_NAMES);
                choice(ui, P_TARGET, &KUBYZ_NAMES);
                capture_row(ui, &s);
            });
            core_gui::section(ui, "Morph", |ui| {
                choice(ui, P_MODE, &MODE_NAMES);
                row(ui, P_VOWELS);
                row(ui, P_BREATH);
                row(ui, P_TILT);
            });
            core_gui::section(ui, "Output", |ui| {
                row(ui, P_MIX);
                row(ui, P_OUTPUT);
            });
            help(ui);
        });
    });
}

fn help(ui: &mut egui::Ui) {
    core_gui::help_block(ui, "morph_help", &[
        ("What it does",
         "The voice keeps its pitch, timing, dynamics and consonants; its timbre moves toward the \
          target as Morph goes 0 → 1. Morph 0 is the dry signal, 43 ms late (the reported latency)."),
        ("Source",
         "Sidechain: anything on the Target input (REAPER: route to track channels 3/4), analysed \
          live — its harmonic profile applies at the voice's pitch. Captured: press Capture while the \
          target plays; the snapshot is saved with the project. Model: the Kubyz preset in Target, \
          or a running Kubyz copied over the mailbox."),
        ("Mode",
         "Harmonic: harmonic k of the voice takes the level of the target's harmonic k. Envelope: \
          the voice's spectral envelope moves to the target's — works on drums and noise. Harmonic \
          falls back to Envelope whenever either side is unpitched."),
        ("Vowels / Breath",
         "Vowels 1 keeps the voice's own formants on the target's harmonics, so the target says the \
          words. Breath is how much of the noise between harmonics survives at Morph 1."),
    ]);
}
