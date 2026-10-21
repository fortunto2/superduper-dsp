//! GUI for SuperDuper Drum — six channel strips + a master row + a
//! pad-light grid that flashes on trigger.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use std::collections::HashMap;
use std::path::PathBuf;

use crate::{pads, voice_name, voice_param_idx, P_DRIVE, P_MASTER, P_NOTE_MAP, P_NOTE_OUT, P_SMP0, P_SRC0, PARAMS, SharedParams};

pub const DEFAULT_WIDTH: u32 = 900;
pub const DEFAULT_HEIGHT: u32 = 720;
pub const MIN_WIDTH: u32 = 600;
pub const MIN_HEIGHT: u32 = 420;
pub const MAX_WIDTH: u32 = 1600;
pub const MAX_HEIGHT: u32 = 1100;

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
    last_pad_decay: f32,
    kit_name: String,
    kit_status: String,
    /// The pad the editor and the browser work on.
    selected: usize,
    /// Browser filters.
    pack_filter: Option<String>,
    search: String,
    dice: u32,
    /// Key filter over the browser; None = automatic (in key when Key is on).
    key_filter: Option<KeyFilter>,
    /// Waveform peaks per sample file, decoded on first view.
    peaks: HashMap<PathBuf, Vec<f32>>,
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P, shared: SharedParams, resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Drum".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let initial_preset_idx = (shared.active_preset.load(std::sync::atomic::Ordering::Relaxed) as usize)
        .min(PRESETS.len().saturating_sub(1));
    let state = GuiState {
        shared, resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(initial_preset_idx),
        preset_names: PRESETS.iter().map(|p| p.name).collect(),
        last_pad_decay: 0.0,
        kit_name: "My Kit".into(),
        kit_status: String::new(),
        selected: 0,
        pack_filter: None,
        search: String::new(),
        dice: 1,
        peaks: HashMap::new(),
        key_filter: None,
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


fn gesture(s: &SharedParams) -> core_gui::GestureBridge<'_> {
    core_gui::GestureBridge { begin: &s.gesture_begin, end: &s.gesture_end }
}

#[derive(Clone, Copy, PartialEq)]
enum KeyFilter {
    All,
    /// Within a semitone of the song key — shifting that little sounds natural.
    InKey,
    Note(u8),
}

const SRC_NAMES: [&str; 3] = ["Synth", "Sample", "Both"];

fn pad_src(s: &SharedParams, pad: usize) -> u8 {
    s.params[P_SRC0 + pad].load(Ordering::Relaxed).round() as u8
}

/// What a pad will play, in a few words.
fn pad_sound(s: &SharedParams, pad: usize) -> String {
    let src = pad_src(s, pad);
    if src == pads::src::SYNTH { return "synth".into(); }
    let idx = s.params[P_SMP0 + pad].load(Ordering::Relaxed).round().max(0.0) as usize;
    let name = pads::pad_entry(s, pad, idx).map_or_else(|| "loading…".into(), |e| short_name(&e.name));
    if src == pads::src::LAYER { format!("{name} + synth") } else { name }
}

/// KSHMR-style names carry the pack prefix; drop it so the sound shows.
fn short_name(name: &str) -> String {
    name.trim_start_matches("KSHMR_").trim_start_matches("909-").replace('_', " ")
}

const KEYS: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

fn draw(ctx: &egui::Context, state: &mut GuiState) {
    let s = state.shared.clone();
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("sdsp> SUPERDUPER DRUM").color(core_gui::GREEN_BRIGHT).monospace().strong());
            ui.label(egui::RichText::new(format!("b{} {}", env!("SDSP_BUILD_NUM"), env!("SDSP_BUILD_DATE"))).color(core_gui::GREEN_DIM).monospace());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut b = s.bypass.load(Ordering::Relaxed);
                if ui.checkbox(&mut b, "bypass").changed() { s.bypass.store(b, Ordering::Relaxed); }
                superduper_synth_core::mix_view::mix_toggle(ui, &s.scope);
            });
        });
        draw_kit_bar(ui, state);
        ui.add_space(4.0);
        draw_pads(ui, state);
        ui.add_space(2.0);
        draw_mini_keyboard(ui, state);
        ui.add_space(6.0);

        let avail = ui.available_width();
        ui.horizontal_top(|ui| {
            ui.allocate_ui(egui::vec2(avail * 0.42, ui.available_height()), |ui| {
                ui.vertical(|ui| draw_editor(ui, state));
            });
            ui.separator();
            ui.allocate_ui(egui::vec2(ui.available_width(), ui.available_height()), |ui| {
                ui.vertical(|ui| draw_browser(ui, state));
            });
        });
    });
}

/// One place to pick a whole kit: synth kits (the factory presets), one
/// sample kit per pack, the kits saved here. 🎲 rerolls the current pack.
fn draw_kit_bar(ui: &mut egui::Ui, state: &mut GuiState) {
    let s = state.shared.clone();
    let packs = pads::packs(&s);
    let kits = pads::kit_files(&pads::kits_dir());
    let pack_now = s.params[crate::P_KIT_PACK].load(Ordering::Relaxed).round() as usize;
    let dice_now = s.params[crate::P_KIT_DICE].load(Ordering::Relaxed).round() as u32;
    let file_now = s.params[crate::P_KIT_FILE].load(Ordering::Relaxed).round() as usize;
    let any_sample = (0..6).any(|v| pad_src(&s, v) != pads::src::SYNTH);
    let current = if pack_now > 0 && any_sample {
        let name = packs.get(pack_now - 1).cloned().unwrap_or_default();
        if dice_now > 0 { format!("{name}  🎲{dice_now}") } else { name }
    } else if file_now > 0 && any_sample {
        kits.get(file_now - 1).and_then(|f| f.file_stem()).map_or("kit".into(), |n| n.to_string_lossy().into_owned())
    } else if any_sample {
        "custom".into()
    } else {
        state.selected_preset.and_then(|i| state.preset_names.get(i)).map_or("synth".into(), |n| format!("{n} (synth)"))
    };

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Kit").color(core_gui::GREEN_BRIGHT).strong());
        egui::ComboBox::from_id_salt("drum_kit").selected_text(current).width(300.0).height(420.0).show_ui(ui, |ui| {
            ui.label(egui::RichText::new("SYNTH KITS").color(core_gui::GREEN_DIM).small());
            for (i, name) in state.preset_names.clone().iter().enumerate() {
                if ui.selectable_label(false, format!("   {name}")).clicked() {
                    crate::apply_preset(&s, i);
                    state.selected_preset = Some(i);
                    for v in 0..6 { pads::set_source(&s, v, pads::src::SYNTH); }
                    for idx in [crate::P_KIT_PACK, crate::P_KIT_FILE] {
                        s.params[idx].store(0.0, Ordering::Relaxed);
                        s.dirty_params[idx].store(true, Ordering::Relaxed);
                    }
                    pads::mark_kit_applied(&s);
                }
            }
            ui.separator();
            ui.label(egui::RichText::new("SAMPLE KITS — one per pack").color(core_gui::GREEN_DIM).small());
            if packs.is_empty() { ui.label("   (library loads when playback starts)"); }
            for (i, p) in packs.iter().enumerate() {
                if ui.selectable_label(pack_now == i + 1 && any_sample, format!("   {p}")).clicked() {
                    pads::request_pack_kit(&s, i, 0);
                }
            }
            ui.separator();
            ui.label(egui::RichText::new(format!("MY KITS — {}", pads::kits_dir().display())).color(core_gui::GREEN_DIM).small());
            if kits.is_empty() { ui.label("   (none yet — Save below)"); }
            for (i, k) in kits.iter().enumerate() {
                let name = k.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if ui.selectable_label(file_now == i + 1 && any_sample, format!("   {name}")).clicked() {
                    pads::request_kit_file(&s, i);
                }
            }
        });
        ui.separator();
        // Song key for kick / 808 / toms.
        let key_now = s.params[crate::P_KEY].load(Ordering::Relaxed).round() as i32;
        let key_label = match key_now {
            0 => "Key: off".to_string(),
            crate::KEY_AUTO => format!("Key: auto {}", pads::song_key(&s).map_or("–".into(), |k| KEYS[k as usize].to_string())),
            k => format!("Key: {}", KEYS[(k.clamp(1, 12) - 1) as usize]),
        };
        egui::ComboBox::from_id_salt("drum_key").selected_text(key_label).width(110.0).show_ui(ui, |ui| {
            let set = |v: i32| {
                s.params[crate::P_KEY].store(v as f32, Ordering::Relaxed);
                s.dirty_params[crate::P_KEY].store(true, Ordering::Relaxed);
            };
            if ui.selectable_label(key_now == 0, "Off").clicked() { set(0); }
            if ui.selectable_label(key_now == crate::KEY_AUTO, "Auto — song key / bass notes").clicked() { set(crate::KEY_AUTO); }
            for (i, n) in KEYS.iter().enumerate() {
                if ui.selectable_label(key_now == i as i32 + 1, *n).clicked() { set(i as i32 + 1); }
            }
        }).response.on_hover_text(format!(
            "Kick, 808 and toms move to this key (±6 st). Auto reads {} — any tool can write a key there (\"A\", \"F#m\") — else the notes this kit passes to a bass synth.",
            superduper_synth_core::drumlib::song_key_path().display()));
        let can_roll = pack_now > 0;
        if ui.add_enabled(can_roll, egui::Button::new("🎲")).on_hover_text("another random kit from this pack").clicked() {
            state.dice = state.dice.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            pads::request_pack_kit(&s, pack_now - 1, 1 + (state.dice >> 8) % 1023);
        }
        ui.separator();
        ui.add(egui::TextEdit::singleline(&mut state.kit_name).desired_width(120.0));
        if ui.button("Save kit").clicked() {
            state.kit_status = match pads::save_kit_sfz(&s, &pads::kits_dir(), state.kit_name.trim()) {
                Ok(p) => format!("saved {}", p.display()),
                Err(e) => e,
            };
        }
    });
    let lib_status = s.pads.status.lock().clone();
    let line = if state.kit_status.is_empty() { lib_status } else { format!("{}   ·   {lib_status}", state.kit_status) };
    ui.label(egui::RichText::new(if line.is_empty() { "Sample library loads when playback starts.".into() } else { line })
        .color(core_gui::GREEN_DIM).monospace().small());
}

/// Six pads: click plays it and selects it for the editor and the browser.
fn draw_pads(ui: &mut egui::Ui, state: &mut GuiState) {
    let s = state.shared.clone();
    let h = 54.0;
    let gap = 6.0;
    let pad_w = ((ui.available_width() - gap * 5.0) / 6.0).max(60.0);
    state.last_pad_decay += ui.input(|i| i.stable_dt).min(0.05);
    let decay = std::mem::take(&mut state.last_pad_decay);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for v in 0..6 {
            let pulse = s.voice_pulse[v].load(Ordering::Relaxed);
            s.voice_pulse[v].store((pulse - decay * 5.0).max(0.0), Ordering::Relaxed);
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(pad_w, h), egui::Sense::click());
            if resp.clicked() {
                state.selected = v;
                s.voice_trigger_request[v].store(0.9, Ordering::Release);
            }
            let src = pad_src(&s, v);
            // Synth = green, Sample = blue, Both = violet.
            let base = match src { 0 => (50.0, 90.0, 55.0), 1 => (45.0, 75.0, 120.0), _ => (90.0, 60.0, 120.0) };
            let b = (pulse + if resp.hovered() { 0.2 } else { 0.0 }).clamp(0.0, 1.0);
            let col = egui::Color32::from_rgb((base.0 + b * 120.0) as u8, (base.1 + b * 120.0) as u8, (base.2 + b * 90.0) as u8);
            let p = ui.painter_at(rect);
            p.rect_filled(rect, 5.0, col);
            if state.selected == v {
                p.rect_stroke(rect, 5.0, egui::Stroke::new(2.0, core_gui::GREEN_BRIGHT), egui::StrokeKind::Inside);
            }
            p.text(rect.center_top() + egui::vec2(0.0, 14.0), egui::Align2::CENTER_CENTER, voice_name(v), egui::FontId::monospace(13.0), egui::Color32::WHITE);
            let sound: String = pad_sound(&s, v).chars().take(22).collect();
            p.text(rect.center_bottom() - egui::vec2(0.0, 13.0), egui::Align2::CENTER_CENTER, sound, egui::FontId::proportional(10.5), egui::Color32::from_gray(220));
        }
    });
}

/// The selected pad: where its sound comes from, its waveform, its knobs.
fn draw_editor(ui: &mut egui::Ui, state: &mut GuiState) {
    let s = state.shared.clone();
    let v = state.selected;
    ui.label(egui::RichText::new(format!("{}  —  {}", voice_name(v), pad_sound(&s, v))).color(core_gui::GREEN_BRIGHT).strong());
    ui.horizontal(|ui| {
        let src = pad_src(&s, v);
        for (val, name) in SRC_NAMES.iter().enumerate() {
            if ui.selectable_label(src == val as u8, *name).clicked() && src != val as u8 {
                pads::set_source(&s, v, val as u8);
                s.gesture_begin[P_SRC0 + v].store(true, Ordering::Relaxed);
                s.gesture_end[P_SRC0 + v].store(true, Ordering::Relaxed);
            }
        }
        ui.separator();
        if ui.button("◀").clicked() { pads::browse(&s, v, -1); }
        if ui.button("▶").clicked() { pads::browse(&s, v, 1); }
    });

    // Waveform of the sample on this pad.
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 70.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 3.0, core_gui::PANEL_BG);
    let path = (pad_src(&s, v) != pads::src::SYNTH).then(|| s.pads.loaded_path[v].lock().clone()).flatten();
    match path {
        Some(path) => {
            let peaks = state.peaks.entry(path.clone()).or_insert_with(|| waveform_peaks(&path, 240));
            let w = rect.width() / peaks.len().max(1) as f32;
            for (i, &pk) in peaks.iter().enumerate() {
                let x = rect.left() + i as f32 * w;
                let hh = pk * rect.height() * 0.48;
                p.line_segment([egui::pos2(x, rect.center().y - hh), egui::pos2(x, rect.center().y + hh)], egui::Stroke::new(w.max(1.0), egui::Color32::from_rgb(110, 170, 230)));
            }
        }
        None => {
            p.text(rect.center(), egui::Align2::CENTER_CENTER, "synth voice — pick a sample on the right", egui::FontId::proportional(11.0), core_gui::GREEN_DIM);
        }
    }
    ui.add_space(4.0);
    for o in 0..4 {
        let i = voice_param_idx(v, o);
        core_gui::dirty_param_row_g(ui, &s.params[i], &PARAMS[i], &s.dirty_params[i], gesture(&s), i);
    }
    egui::CollapsingHeader::new("Master").default_open(false).show(ui, |ui| {
        core_gui::dirty_param_row_g(ui, &s.params[P_DRIVE], &PARAMS[P_DRIVE], &s.dirty_params[P_DRIVE], gesture(&s), P_DRIVE);
        core_gui::dirty_param_row_g(ui, &s.params[P_MASTER], &PARAMS[P_MASTER], &s.dirty_params[P_MASTER], gesture(&s), P_MASTER);
        core_gui::dirty_toggle_row_g(ui, &s.params[P_NOTE_OUT], &PARAMS[P_NOTE_OUT], &s.dirty_params[P_NOTE_OUT], gesture(&s), P_NOTE_OUT);
        core_gui::dirty_choice_row_g(ui, &s.params[P_NOTE_MAP], &PARAMS[P_NOTE_MAP], &["Auto", "White Keys", "GM"], &s.dirty_params[P_NOTE_MAP], gesture(&s), P_NOTE_MAP);
        ui.label(egui::RichText::new("Auto: GM (36 kick, 38 snare, 42/46 hats, 39 clap) + white keys C–A. Other notes go out to a chained synth.")
            .color(core_gui::GREEN_DIM).small());
    });
}

/// Sounds for the selected pad's type, filtered by pack and search. Click =
/// put it on the pad and hear it.
fn draw_browser(ui: &mut egui::Ui, state: &mut GuiState) {
    let s = state.shared.clone();
    let v = state.selected;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("Sounds for {}", voice_name(v))).color(core_gui::GREEN_BRIGHT).strong());
        egui::ComboBox::from_id_salt("drum_pack_filter")
            .selected_text(state.pack_filter.clone().unwrap_or_else(|| "all packs".into())).width(160.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(state.pack_filter.is_none(), "all packs").clicked() { state.pack_filter = None; }
                for p in pads::packs(&s) {
                    if ui.selectable_label(state.pack_filter.as_deref() == Some(&p), &p).clicked() { state.pack_filter = Some(p); }
                }
            });
        ui.add(egui::TextEdit::singleline(&mut state.search).hint_text("search").desired_width(100.0));
        if ui.small_button("Rescan").clicked() { s.pads.scanned.store(false, Ordering::Relaxed); }
    });
    let rows: Vec<(usize, superduper_synth_core::drumlib::DrumSample)> = {
        let lib = s.pads.library.lock();
        let lists = s.pads.lists.lock();
        let q = state.search.to_lowercase();
        lists[v].iter().enumerate()
            .filter_map(|(i, &li)| lib.get(li).map(|e| (i, e.clone())))
            .filter(|(_, e)| state.pack_filter.as_ref().is_none_or(|p| &e.pack == p))
            .filter(|(_, e)| q.is_empty() || e.name.to_lowercase().contains(&q))
            .collect()
    };
    let song = pads::effective_key(&s).filter(|_| v == 0 || v == 5);
    let shift_of = |e: &superduper_synth_core::drumlib::DrumSample| -> Option<i32> {
        let k = e.key.filter(|_| superduper_synth_core::drumlib::is_tonal(e.class))?;
        Some(superduper_synth_core::drumlib::key_shift(k, song?))
    };
    // Key chips: only on the pads that have a pitch (kick, tom/perc).
    let tonal_pad = v == 0 || v == 5;
    let filter = match state.key_filter {
        Some(KeyFilter::InKey) if song.is_none() => KeyFilter::All,
        Some(f) => f,
        None if song.is_some() && v == 0 => KeyFilter::InKey,
        None => KeyFilter::All,
    };
    if tonal_pad {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            if ui.selectable_label(filter == KeyFilter::All, "all").clicked() { state.key_filter = Some(KeyFilter::All); }
            let in_key = ui.add_enabled(song.is_some(), egui::Button::selectable(filter == KeyFilter::InKey,
                song.map_or("in key".to_string(), |k| format!("in {} ±1", KEYS[k as usize]))));
            if in_key.clicked() { state.key_filter = Some(KeyFilter::InKey); }
            in_key.on_disabled_hover_text("set Key (top bar) to Auto or a note first");
            ui.separator();
            for (i, n) in KEYS.iter().enumerate() {
                if ui.selectable_label(filter == KeyFilter::Note(i as u8), *n).clicked() {
                    state.key_filter = Some(KeyFilter::Note(i as u8));
                }
            }
        });
    }
    let mut rows: Vec<_> = rows.into_iter().filter(|(_, e)| match (tonal_pad, filter) {
        (false, _) | (_, KeyFilter::All) => true,
        (_, KeyFilter::InKey) => shift_of(e).is_some_and(|d| d.abs() <= 1),
        (_, KeyFilter::Note(k)) => e.key == Some(k),
    }).collect();
    if song.is_some() {
        // Least shifting at the top, unknown pitch last.
        rows.sort_by_key(|(_, e)| shift_of(e).map_or(99, |d| d.abs()));
    }
    let cur = s.params[P_SMP0 + v].load(Ordering::Relaxed).round() as usize;
    let on_sample = pad_src(&s, v) != pads::src::SYNTH;
    ui.label(egui::RichText::new(format!("{} sounds", rows.len())).color(core_gui::GREEN_DIM).small());
    egui::ScrollArea::vertical().id_salt("drum_browser").auto_shrink([false, false]).show_rows(ui, 18.0, rows.len(), |ui, range| {
        for (i, e) in &rows[range] {
            let key = e.key.map_or(String::new(), |k| format!("  {}{}", KEYS[k as usize % 12], if e.key_guessed { "~" } else { "" }));
            let key = match shift_of(e) { Some(0) => format!("{key} ✓"), Some(d) => format!("{key} {d:+}"), None => key };
            let text = format!("{}{}   {:.2}s   · {}", short_name(&e.name), key, e.duration_s, e.pack);
            if ui.selectable_label(on_sample && *i == cur, text).clicked() {
                pads::assign(&s, v, *i);
            }
        }
    });
}

/// Peak envelope of a file for the waveform view (GUI thread, small files).
fn waveform_peaks(path: &std::path::Path, bins: usize) -> Vec<f32> {
    let Ok(buf) = superduper_synth_core::sfz::load::decode_file(path) else { return vec![0.0; bins] };
    let mono = superduper_synth_core::drumlib::mono_of(&buf);
    let per = (mono.len() / bins).max(1);
    let peaks: Vec<f32> = mono.chunks(per).map(|c| c.iter().fold(0f32, |m, &x| m.max(x.abs()))).collect();
    let max = peaks.iter().cloned().fold(1e-6f32, f32::max);
    peaks.into_iter().map(|p| p / max).collect()
}

fn draw_mini_keyboard(ui: &mut egui::Ui, state: &GuiState) {
    let total_w = ui.available_width();
    let h = 40.0;
    let (rect, _resp) = ui.allocate_exact_size(
        egui::vec2(total_w, h),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, core_gui::PANEL_BG);

    // 7 white keys side by side.
    let white_count = 7.0;
    let white_w = rect.width() / white_count;
    // Drum voice that each white-key class maps to.
    let white_voice: [(&str, Option<usize>); 7] = [
        ("C·Kick",  Some(0)),
        ("D·Snare", Some(1)),
        ("E·HHc",   Some(2)),
        ("F·HHo",   Some(3)),
        ("G·Clap",  Some(4)),
        ("A·Cowb",  Some(5)),
        ("B·pass",  None),
    ];
    let inv_id = ui.next_auto_id().with("drum_keyboard");
    let _ = inv_id; // suppress unused warning; we use explicit ids below.

    for (i, (label, voice)) in white_voice.iter().enumerate() {
        let key_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left() + i as f32 * white_w, rect.top()),
            egui::vec2(white_w - 1.0, rect.height()),
        );
        let resp = ui.interact(
            key_rect,
            egui::Id::new(("drum_key_white", i)),
            egui::Sense::click(),
        );
        let is_voice = voice.is_some();
        let lit = if let Some(v) = voice {
            state.shared.voice_pulse[*v].load(Ordering::Relaxed)
        } else {
            0.0
        };
        let hover = if resp.hovered() && is_voice { 0.3 } else { 0.0 };
        let bright = (lit + hover).clamp(0.0, 1.0);
        // White keys: warm cream when armed, brighter when lit.
        let base = if is_voice { (230.0, 230.0, 200.0) } else { (140.0, 140.0, 130.0) };
        let r = (base.0 + bright * (255.0 - base.0)) as u8;
        let g = (base.1 + bright * (255.0 - base.1)) as u8;
        let b = (base.2 + bright * (255.0 - base.2)) as u8;
        painter.rect_filled(key_rect, 2.0, egui::Color32::from_rgb(r, g, b));
        painter.rect_stroke(
            key_rect, 2.0,
            egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
            egui::StrokeKind::Inside,
        );
        // Label rotated upright in the centre.
        painter.text(
            egui::pos2(key_rect.center().x, key_rect.bottom() - 10.0),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::monospace(10.0),
            if is_voice { egui::Color32::from_rgb(40, 40, 40) }
            else { egui::Color32::from_rgb(90, 90, 90) },
        );
        if resp.clicked() {
            if let Some(v) = voice {
                state.shared.voice_trigger_request[*v].store(0.9, Ordering::Release);
            }
        }
    }

    // Black keys — drawn ON TOP of the whites at the standard
    // C#-D# (skip) F#-G#-A# (skip) layout. All pass-through.
    let black_w = white_w * 0.55;
    let black_h = rect.height() * 0.62;
    // x-offsets (in white-key positions from C): C# = 1.0 - half_black,
    // D# = 2.0 - half_black, F# = 4.0 - half_black, etc.
    let black_positions = [1.0_f32, 2.0, 4.0, 5.0, 6.0];
    let black_labels = ["C#", "D#", "F#", "G#", "A#"];
    for (i, &pos) in black_positions.iter().enumerate() {
        let cx = rect.left() + pos * white_w;
        let key_rect = egui::Rect::from_min_size(
            egui::pos2(cx - black_w * 0.5, rect.top()),
            egui::vec2(black_w, black_h),
        );
        let resp = ui.interact(
            key_rect,
            egui::Id::new(("drum_key_black", i)),
            egui::Sense::hover(),
        );
        let hover = if resp.hovered() { 0.25 } else { 0.0 };
        let r = (32.0 + hover * 40.0) as u8;
        let g = (40.0 + hover * 60.0) as u8;
        let b = (32.0 + hover * 40.0) as u8;
        painter.rect_filled(key_rect, 2.0, egui::Color32::from_rgb(r, g, b));
        painter.text(
            egui::pos2(key_rect.center().x, key_rect.bottom() - 6.0),
            egui::Align2::CENTER_CENTER,
            black_labels[i],
            egui::FontId::monospace(8.0),
            egui::Color32::from_rgb(150, 150, 150),
        );
    }
}
