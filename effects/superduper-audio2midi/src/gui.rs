//! GUI: status, the controls, a piano roll and, in Guitar mode, a six-string tab.

use crate::notes::Record;
use crate::{request_analyse, request_clear, SharedParams, PARAMS, P_AUTO, P_CAPO, P_CAPTURE, P_FRAME_THR, P_MAX_FRET, P_MODE, P_ONSET_THR, P_PLAY, P_THRU};
use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Sense, Stroke};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use superduper_synth_core::gui as core_gui;

pub const DEFAULT_WIDTH: u32 = 760;
pub const DEFAULT_HEIGHT: u32 = 640;
pub const MIN_WIDTH: u32 = 520;
pub const MIN_HEIGHT: u32 = 420;
pub const MAX_WIDTH: u32 = 1800;
pub const MAX_HEIGHT: u32 = 1400;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

struct GuiState {
    shared: SharedParams,
    resize: ResizeBridge,
    applied_size: (u32, u32),
    selected_preset: Option<usize>,
    /// Visible time window (song seconds); `None` = fit the whole transcription.
    view: Option<(f64, f64)>,
}

pub fn open_window<P: HasRawWindowHandle>(parent: &P, shared: SharedParams, resize: ResizeBridge) -> WindowHandle {
    let (w, h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Audio2MIDI".to_string(),
        size: Size::new(w as f64, h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let state = GuiState { shared, resize, applied_size: (w, h), selected_preset: None, view: None };
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

fn gb(s: &SharedParams) -> core_gui::GestureBridge<'_> {
    core_gui::GestureBridge { begin: &s.gesture_begin, end: &s.gesture_end }
}

fn row(ui: &mut egui::Ui, s: &SharedParams, i: usize) {
    core_gui::dirty_param_row_g(ui, &s.params[i], &PARAMS[i], &s.dirty_params[i], gb(s), i);
}

fn toggle(ui: &mut egui::Ui, s: &SharedParams, i: usize) {
    core_gui::dirty_toggle_row_g(ui, &s.params[i], &PARAMS[i], &s.dirty_params[i], gb(s), i);
}

fn reveal(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg("/select,").arg(path).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
}

fn state_line(s: &SharedParams) -> (String, Color32) {
    let n = s.note_count.load(Ordering::Relaxed);
    if s.analysing.load(Ordering::Relaxed) {
        (format!("analysing {:.0}%", s.progress.load(Ordering::Relaxed) * 100.0), Color32::from_rgb(230, 190, 80))
    } else if s.capturing.load(Ordering::Relaxed) {
        (format!("capturing {:.1} s", s.captured_secs.load(Ordering::Relaxed)), core_gui::GREEN_BRIGHT)
    } else if n > 0 {
        (format!("done: {n} notes"), core_gui::GREEN_BRIGHT)
    } else if s.captured_secs.load(Ordering::Relaxed) > 0.0 {
        (format!("idle, {:.1} s captured", s.captured_secs.load(Ordering::Relaxed)), core_gui::GREEN_DIM)
    } else {
        ("idle: press play".to_string(), core_gui::GREEN_DIM)
    }
}

fn draw(ctx: &egui::Context, state: &mut GuiState) {
    let s = state.shared.clone();
    egui::CentralPanel::default().show(ctx, |ui| {
        core_gui::top_bar_mix(
            &s.scope, ui, "SuperDuper Audio2MIDI", env!("SDSP_BUILD_NUM"), env!("SDSP_BUILD_DATE"),
            &s.bypass, "a2m_preset_combo", &[], &mut state.selected_preset,
        );
        core_gui::ab_init_bar(ui, &s.ab_snapshot, &s.params, PARAMS, &s.dirty_params);

        let (txt, col) = state_line(&s);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(txt).color(col).monospace());
            if s.analysing.load(Ordering::Relaxed) {
                ui.add(egui::ProgressBar::new(s.progress.load(Ordering::Relaxed)).desired_width(160.0));
            }
        });
        let status = s.status.lock().clone();
        if !status.is_empty() {
            ui.label(egui::RichText::new(status).small().color(core_gui::GREEN_DIM));
        }

        ui.horizontal(|ui| {
            if ui.add_enabled(!s.analysing.load(Ordering::Relaxed), egui::Button::new("Analyse")).clicked() {
                request_analyse(&s);
            }
            if ui.button("Clear").clicked() {
                request_clear(&s);
            }
            let rec = s.record.lock().clone();
            if let Some(p) = rec.as_ref().and_then(|r| r.midi_path.clone()) {
                if ui.button("Reveal .mid").clicked() {
                    reveal(&p);
                }
                ui.label(egui::RichText::new(p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).small());
            } else if ui.button("Open folder").clicked() {
                let _ = std::fs::create_dir_all(&s.out_dir);
                reveal(&s.out_dir.join("."));
            }
        });

        core_gui::section(ui, "Transcribe", |ui| {
            core_gui::dirty_choice_row_g(ui, &s.params[P_MODE], &PARAMS[P_MODE], &["Any", "Guitar"], &s.dirty_params[P_MODE], gb(&s), P_MODE);
            if s.params[P_MODE].load(Ordering::Relaxed) >= 0.5 {
                row(ui, &s, P_CAPO);
                row(ui, &s, P_MAX_FRET);
            }
            row(ui, &s, P_ONSET_THR);
            row(ui, &s, P_FRAME_THR);
            toggle(ui, &s, P_CAPTURE);
            toggle(ui, &s, P_AUTO);
        });
        core_gui::section(ui, "Play", |ui| {
            toggle(ui, &s, P_PLAY);
            row(ui, &s, P_THRU);
        });

        let rec = s.record.lock().clone();
        match rec {
            Some(rec) if !rec.notes.is_empty() => {
                let guitar = rec.is_guitar() && rec.notes.iter().any(|n| n.string.is_some());
                let full = ui.available_height();
                let roll_h = if guitar { full * 0.5 } else { full - 8.0 }.max(120.0);
                piano_roll(ui, state, &rec, roll_h);
                if guitar {
                    tab_view(ui, state, &rec, (ui.available_height() - 8.0).max(100.0));
                }
            }
            _ => {
                ui.add_space(8.0);
                ui.label("Press play on a track with audio. When the transport stops the take is transcribed (Auto), or press Analyse. Notes appear here and are saved as .mid.");
            }
        }
    });
}

/// Pan with drag, zoom with the scroll wheel, double-click to fit.
fn view_window(ui: &egui::Ui, resp: &egui::Response, rect: Rect, state: &mut GuiState, rec: &Record) -> (f64, f64) {
    let all = (rec.notes.iter().map(|n| n.start).fold(f64::MAX, f64::min), rec.notes.iter().map(|n| n.end).fold(0.0, f64::max));
    let fit = (all.0 - 0.1, (all.1 - all.0 + 0.2).max(1.0));
    if resp.double_clicked() {
        state.view = None;
        return fit;
    }
    let (mut t0, mut len) = state.view.unwrap_or(fit);
    let mut changed = false;
    if resp.dragged() {
        t0 -= resp.drag_delta().x as f64 / rect.width() as f64 * len;
        changed = true;
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll != 0.0 {
            let anchor = ui.input(|i| i.pointer.hover_pos()).map_or(0.5, |p| ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64);
            let new_len = (len * (-scroll as f64 * 0.004).exp()).clamp(0.5, fit.1 * 4.0);
            t0 += (len - new_len) * anchor;
            len = new_len;
            changed = true;
        }
    }
    if changed {
        state.view = Some((t0, len));
    }
    (t0, len)
}

fn playhead(ui: &egui::Ui, rect: Rect, s: &SharedParams, t0: f64, len: f64) {
    let p = s.play_pos.load(Ordering::Relaxed) as f64;
    if p >= t0 && p <= t0 + len {
        let x = rect.left() + ((p - t0) / len) as f32 * rect.width();
        ui.painter_at(rect).line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0_f32, Color32::from_rgb(240, 240, 240)));
    }
}

fn note_name(m: u8) -> String {
    const N: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", N[(m % 12) as usize], m as i32 / 12 - 1)
}

fn string_color(string: Option<u8>) -> Color32 {
    const C: [Color32; 6] = [
        Color32::from_rgb(240, 120, 120), Color32::from_rgb(240, 180, 90), Color32::from_rgb(230, 230, 100),
        Color32::from_rgb(110, 220, 130), Color32::from_rgb(100, 190, 240), Color32::from_rgb(170, 140, 240),
    ];
    string.map_or(core_gui::GREEN_BRIGHT, |s| C[(s as usize).clamp(1, 6) - 1])
}

fn piano_roll(ui: &mut egui::Ui, state: &mut GuiState, rec: &Arc<Record>, height: f32) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
    let (t0, len) = view_window(ui, &resp, rect, state, rec);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_rgb(14, 18, 16));
    let (lo, hi) = rec.notes.iter().fold((127u8, 0u8), |(l, h), n| (l.min(n.midi), h.max(n.midi)));
    let (lo, hi) = (lo.saturating_sub(1), (hi + 1).min(127));
    let rows = (hi - lo + 1) as f32;
    let rh = rect.height() / rows;
    for m in lo..=hi {
        let y = rect.bottom() - (m - lo + 1) as f32 * rh;
        if matches!(m % 12, 1 | 3 | 6 | 8 | 10) {
            painter.rect_filled(Rect::from_min_size(pos2(rect.left(), y), vec2(rect.width(), rh)), 0.0, Color32::from_rgba_unmultiplied(255, 255, 255, 8));
        }
        if m % 12 == 0 && rh > 6.0 {
            painter.text(pos2(rect.left() + 3.0, y + rh * 0.5), Align2::LEFT_CENTER, note_name(m), FontId::monospace(9.0), Color32::from_rgb(110, 130, 115));
        }
    }
    let x_of = |t: f64| rect.left() + ((t - t0) / len) as f32 * rect.width();
    let step = [0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0].into_iter().find(|s| len / s < 12.0).unwrap_or(120.0);
    let mut t = (t0 / step).ceil() * step;
    while t < t0 + len {
        let x = x_of(t);
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 14)));
        painter.text(pos2(x + 2.0, rect.top() + 2.0), Align2::LEFT_TOP, format!("{t:.1}s"), FontId::monospace(8.0), Color32::from_rgb(110, 130, 115));
        t += step;
    }
    for n in &rec.notes {
        if n.end < t0 || n.start > t0 + len {
            continue;
        }
        let y = rect.bottom() - (n.midi - lo + 1) as f32 * rh;
        let x0 = x_of(n.start).max(rect.left());
        let r = Rect::from_min_max(pos2(x0, y + 0.5), pos2(x_of(n.end).min(rect.right()).max(x0 + 2.0), y + rh - 0.5));
        let a = 90 + (n.velocity as u32 * 165 / 127) as u8;
        let c = string_color(n.string);
        painter.rect_filled(r, 1.5, Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a));
    }
    playhead(ui, rect, &state.shared, t0, len);
    resp.on_hover_text("drag: pan, wheel: zoom, double-click: fit");
}

fn tab_view(ui: &mut egui::Ui, state: &mut GuiState, rec: &Arc<Record>, height: f32) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click_and_drag());
    let (t0, len) = view_window(ui, &resp, rect, state, rec);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, Color32::from_rgb(14, 18, 16));
    let gap = rect.height() / 7.0;
    let y_of = |string: u8| rect.top() + gap * string as f32; // string 1 on top
    let x_of = |t: f64| rect.left() + ((t - t0) / len) as f32 * rect.width();
    for st in 1..=6u8 {
        painter.line_segment([pos2(rect.left() + 14.0, y_of(st)), pos2(rect.right(), y_of(st))], Stroke::new(1.0_f32, Color32::from_rgb(70, 85, 75)));
        painter.text(pos2(rect.left() + 3.0, y_of(st)), Align2::LEFT_CENTER, st.to_string(), FontId::monospace(9.0), Color32::from_rgb(110, 130, 115));
    }
    for n in &rec.notes {
        let (Some(st), Some(fret)) = (n.string, n.fret) else { continue };
        if n.start < t0 || n.start > t0 + len {
            continue;
        }
        let p = pos2(x_of(n.start), y_of(st));
        painter.rect_filled(Rect::from_center_size(p, vec2(if fret > 9 { 17.0 } else { 11.0 }, 12.0)), 2.0, Color32::from_rgb(14, 18, 16));
        painter.text(p, Align2::CENTER_CENTER, fret.to_string(), FontId::monospace(11.0), string_color(Some(st)));
    }
    playhead(ui, rect, &state.shared, t0, len);
    resp.on_hover_text("string 1 = high E on top; the number is the fret (capo counted from the nut)");
}
