//! GUI for SuperDuper Piano — pack browser + play controls.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::{
    pick_instrument, refresh_library, SharedParams, PARAMS, P_HOLD, P_OUTPUT, P_RELEASE, P_TONE,
    P_TRANSPOSE, P_TUNE, P_VEL_CURVE, P_WIDTH,
};

pub const DEFAULT_WIDTH: u32 = 640;
pub const DEFAULT_HEIGHT: u32 = 700;
pub const MIN_WIDTH: u32 = 480;
pub const MIN_HEIGHT: u32 = 480;
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
    search: String,
    selected_preset: Option<usize>,
}

pub fn open_window<P: HasRawWindowHandle>(parent: &P, shared: SharedParams, resize: ResizeBridge) -> WindowHandle {
    let (w, h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Piano".to_string(),
        size: Size::new(w as f64, h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let state = GuiState { shared, resize, applied_size: (w, h), search: String::new(), selected_preset: None };
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

fn row(ui: &mut egui::Ui, s: &SharedParams, i: usize) {
    core_gui::dirty_param_row_g(
        ui, &s.params[i], &PARAMS[i], &s.dirty_params[i],
        core_gui::GestureBridge { begin: &s.gesture_begin, end: &s.gesture_end }, i,
    );
}

fn open_in_finder(path: &std::path::Path) {
    let _ = std::fs::create_dir_all(path);
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

fn draw(ctx: &egui::Context, state: &mut GuiState) {
    let s = state.shared.clone();
    egui::CentralPanel::default().show(ctx, |ui| {
        core_gui::top_bar(
            ui, "SuperDuper Piano", env!("SDSP_BUILD_NUM"), env!("SDSP_BUILD_DATE"),
            &s.bypass, "piano_preset_combo", &[], &mut state.selected_preset,
        );
        core_gui::ab_init_bar(ui, &s.ab_snapshot, &s.params, PARAMS, &s.dirty_params);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 60.0), egui::Sense::hover());
        core_gui::draw_spectrum_strip(ui, &s.scope, rect, s.host_sr.load(Ordering::Relaxed));

        // Status: what is loaded / loading.
        let info = s.info.lock().clone();
        let status = s.status.lock().clone();
        if s.loading.load(Ordering::Relaxed) {
            let done = s.progress.done.load(Ordering::Relaxed);
            let total = s.progress.total.load(Ordering::Relaxed).max(1);
            ui.add(egui::ProgressBar::new(done as f32 / total as f32).text(format!("{status}  {done}/{total}")));
        } else if !s.activated.load(Ordering::Relaxed) {
            ui.label("Start playback (or arm the track) to load the instrument.");
        } else {
            ui.label(egui::RichText::new(status).monospace());
        }
        if !info.missing.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(230, 120, 80), format!("{} samples missing — e.g. {}", info.missing.len(), info.missing[0]));
        }
        if !info.unsupported.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(200, 180, 80), format!("ignored opcodes: {}", info.unsupported.join(", ")));
        }
        ui.label(format!("Voices: {}", s.active_voices.load(Ordering::Relaxed)));

        draw_keyboard(ui, &s, info.key_range);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Instruments", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Find:");
                    ui.text_edit_singleline(&mut state.search);
                    if ui.button("Rescan").clicked() { refresh_library(&s); }
                    if ui.button("Open folder").clicked() { open_in_finder(&s.pack_root); }
                });
                let lib = s.library.lock().clone();
                let cur = s.current_index.load(Ordering::Relaxed);
                if lib.is_empty() {
                    ui.label(format!("No .sfz found in {}. Drop a pack folder there and press Rescan.", s.pack_root.display()));
                }
                let q = state.search.to_lowercase();
                egui::ScrollArea::vertical().id_salt("piano_lib").max_height(220.0).show(ui, |ui| {
                    let mut last_pack = "";
                    for (i, e) in lib.iter().enumerate() {
                        let label = format!("{} / {}", e.pack, e.name);
                        if !q.is_empty() && !label.to_lowercase().contains(&q) { continue; }
                        if e.pack != last_pack {
                            ui.label(egui::RichText::new(&e.pack).strong());
                            last_pack = &e.pack;
                        }
                        if ui.selectable_label(i as i32 == cur, format!("   {}", e.name)).clicked() {
                            pick_instrument(&s, i);
                        }
                    }
                });
            });
            core_gui::section(ui, "Play", |ui| {
                row(ui, &s, P_TRANSPOSE);
                row(ui, &s, P_TUNE);
                row(ui, &s, P_VEL_CURVE);
                row(ui, &s, P_RELEASE);
                core_gui::dirty_toggle_row_g(
                    ui, &s.params[P_HOLD], &PARAMS[P_HOLD], &s.dirty_params[P_HOLD],
                    core_gui::GestureBridge { begin: &s.gesture_begin, end: &s.gesture_end }, P_HOLD,
                );
            });
            core_gui::section(ui, "Colour", |ui| {
                row(ui, &s, P_TONE);
                row(ui, &s, P_WIDTH);
                row(ui, &s, P_OUTPUT);
            });
            core_gui::help_block_with_links(ui, "piano_help", &[
                ("Adding instruments",
                 "Any folder with an .sfz file under ~/Music/SFZ becomes an entry: the first folder is the pack, the .sfz name is the instrument. Drop it in, press Rescan. Samples stay where the .sfz expects them (usually a Samples/ folder next to it). FLAC, WAV and OGG are read.",
                 &[("Salamander Grand (CC-BY)", "https://github.com/sfzinstruments/SalamanderGrandPiano"),
                   ("sfzinstruments — free SFZ libraries", "https://sfzinstruments.github.io"),
                   ("SFZ format reference", "https://sfzformat.com")]),
                ("Play controls",
                 "Transpose moves which key is played, so every note keeps its own recording. Vel Curve: −1 needs a hard touch, +1 makes soft playing louder. Release scales every release time. Hold latches the sustain pedal (useful without a pedal). The instrument's own controls (Salamander: hammer noise, string resonance, release on CC 20/21/22/72) answer to MIDI CC.",
                 &[]),
                ("When something is missing",
                 "The status lines name missing sample files and SFZ opcodes this player ignores. An ignored opcode means the pack will play, but not exactly as its author intended.",
                 &[]),
            ]);
        });
    });
}

fn is_black(key: u8) -> bool { matches!(key % 12, 1 | 3 | 6 | 8 | 10) }

/// 88 keys, A0..C8. Held keys light bright, keys still sounding (pedal,
/// release tail) light dim, keys the instrument has no samples for are
/// greyed. Press and drag to play; lower on the key = louder.
fn draw_keyboard(ui: &mut egui::Ui, s: &SharedParams, range: Option<(u8, u8)>) {
    const LO: u8 = 21;
    const HI: u8 = 108;
    let pedal = s.pedal_on.load(Ordering::Relaxed);
    ui.horizontal(|ui| {
        let (txt, col) = if pedal { ("● PEDAL", core_gui::GREEN_BRIGHT) } else { ("○ pedal", core_gui::GREEN_DIM) };
        ui.label(egui::RichText::new(txt).color(col).monospace());
        ui.label(egui::RichText::new("click / drag keys to play — lower = louder").color(core_gui::GREEN_DIM).small());
    });
    let whites: Vec<u8> = (LO..=HI).filter(|k| !is_black(*k)).collect();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 84.0), egui::Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let ww = rect.width() / whites.len() as f32;
    let bw = ww * 0.62;
    let bh = rect.height() * 0.6;
    let white_x = |key: u8| whites.iter().position(|&w| w == key).map(|i| rect.left() + i as f32 * ww);
    let black_rect = |key: u8| {
        white_x(key - 1).map(|x| egui::Rect::from_min_size(egui::pos2(x + ww - bw * 0.5, rect.top()), egui::vec2(bw, bh)))
    };
    let in_range = |k: u8| range.is_none_or(|(a, b)| k >= a && k <= b);
    let fill = |k: u8, black: bool| {
        match s.key_state[k as usize].load(Ordering::Relaxed) {
            2 => egui::Color32::from_rgb(90, 230, 120),
            1 => if black { egui::Color32::from_rgb(40, 120, 60) } else { egui::Color32::from_rgb(170, 225, 180) },
            _ if !in_range(k) => if black { egui::Color32::from_rgb(45, 45, 45) } else { egui::Color32::from_rgb(120, 120, 115) },
            _ => if black { egui::Color32::from_rgb(25, 28, 25) } else { egui::Color32::from_rgb(235, 235, 225) },
        }
    };
    for (i, &k) in whites.iter().enumerate() {
        let r = egui::Rect::from_min_size(egui::pos2(rect.left() + i as f32 * ww, rect.top()), egui::vec2(ww - 1.0, rect.height()));
        painter.rect_filled(r, 2.0, fill(k, false));
        if k % 12 == 0 {
            painter.text(egui::pos2(r.center().x, r.bottom() - 8.0), egui::Align2::CENTER_CENTER,
                format!("C{}", k / 12 - 1), egui::FontId::monospace(8.0), egui::Color32::from_rgb(70, 70, 70));
        }
    }
    for k in (LO..=HI).filter(|k| is_black(*k)) {
        if let Some(r) = black_rect(k) { painter.rect_filled(r, 2.0, fill(k, true)); }
    }

    // Pointer → key (black keys sit on top, so they are hit-tested first).
    let mut target: i32 = -1;
    if resp.is_pointer_button_down_on() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let black = (LO..=HI).filter(|k| is_black(*k)).find(|&k| black_rect(k).is_some_and(|r| r.contains(pos)));
            let key = black.or_else(|| {
                let i = ((pos.x - rect.left()) / ww).floor().max(0.0) as usize;
                whites.get(i.min(whites.len() - 1)).copied()
            });
            if let Some(k) = key {
                target = k as i32;
                let depth = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
                s.gui_velocity.store(0.25 + 0.75 * depth, Ordering::Relaxed);
            }
        }
    }
    s.gui_key.store(target, Ordering::Relaxed);
}
