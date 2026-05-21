//! GUI for SuperDuper Sampler — sample picker dropdown + ADSR knobs.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::bank::pitch_to_note_name;
use crate::{
    add_sample_root, cutoff_units_to_hz, pick_sample, refresh_library, remove_sample_root,
    reset_sample_roots, P_ATTACK, P_CUTOFF, P_DECAY, P_ENV_CUTOFF, P_FILTER_TYPE, P_FINE,
    P_LOOP, P_LOOP_END, P_LOOP_START, P_OUTPUT, P_RELEASE, P_RESO, P_REVERSE, P_ROOT,
    P_SUSTAIN, P_TRIM_END, P_TRIM_START, P_TUNE, P_VEL_AMP, P_VEL_CUTOFF, PARAMS, SharedParams,
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
    /// Last status message — set by Pick / Refresh / folder actions.
    status: String,
    /// Currently-dragged waveform marker (Some = drag in progress).
    dragging: Option<Marker>,
    /// Active pack filter — None = show all packs in the sample
    /// dropdown; Some(name) = show only that pack's WAVs.
    pack_filter: Option<String>,
    /// Text input buffer for the "add new sample folder" field.
    folder_input: String,
    /// Whether the "Sample Folders" panel is expanded — collapsed
    /// by default to stay out of the way during normal play.
    folders_expanded: bool,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Marker { TrimStart, TrimEnd, LoopStart, LoopEnd }

/// Read arrow-key press out of the egui input queue. Skips only when
/// a TEXT widget actually wants the key (e.g. the Add-folder field
/// is being edited) — sliders/combos having focus must NOT block the
/// arrows, otherwise picking one sample disables further keyboard
/// browsing. `wants_keyboard_input` is exactly the right signal for
/// "egui is in text-edit mode".
fn ctx_input_pressed_arrow(ctx: &egui::Context, key: egui::Key) -> bool {
    if ctx.wants_keyboard_input() { return false; }
    ctx.input(|i| i.key_pressed(key))
}

/// Request an audition trigger at the current Root key. Reads Root
/// out of the param atomics so the auditioned pitch matches what
/// the host will play when a MIDI NoteOn comes in at Root.
fn audition_at_root(shared: &SharedParams) {
    let root = shared.params[crate::P_ROOT].load(Ordering::Relaxed).round() as i32;
    let key = root.clamp(0, 127);
    shared.audition_request.store(key, Ordering::Release);
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
        dragging: None,
        pack_filter: None,
        folder_input: String::new(),
        folders_expanded: false,
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

        // Big waveform display with trim + loop markers, ADSR overlay.
        // Takes up most of the top half of the GUI — central focus of
        // a sampler, much more useful than the spectrum strip here.
        let (wave_rect, wave_resp) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 140.0),
            egui::Sense::click_and_drag(),
        );
        draw_waveform(ui, state, wave_rect, &wave_resp);
        // A plain click on the waveform body — not on a marker drag —
        // auditions the sample at the current Root key. `state.dragging`
        // is set by handle_marker_drag() inside draw_waveform when a
        // grab-handle is captured, so we use it to distinguish "click
        // to play" from "click to grab a marker".
        if wave_resp.clicked() && state.dragging.is_none() {
            audition_at_root(&state.shared);
        }

        ui.add_space(4.0);
        let (scope_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 32.0),
            egui::Sense::hover(),
        );
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, scope_rect, 48_000.0);

        ui.add_space(6.0);

        // Sample picker — two-level (Pack → Sample). Library lock
        // is held briefly to snapshot entries; audio thread never
        // blocks on it.
        let (entries, current_idx) = {
            let lib = state.shared.library.lock();
            let entries: Vec<(String, String)> = lib.iter()
                .map(|s| {
                    let stem = s.path.file_stem()
                        .map(|x| x.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "?".into());
                    (s.pack.clone(), stem)
                }).collect();
            let idx = state.shared.current_index.load(Ordering::Relaxed);
            (entries, idx)
        };
        // Unique pack list in scan order.
        let mut packs: Vec<String> = Vec::new();
        for (p, _) in &entries {
            if !packs.contains(p) { packs.push(p.clone()); }
        }

        // Pack selector + Rescan + Reset on its own row so the Pack
        // combo doesn't fight Sample for horizontal space.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Pack:").color(core_gui::GREEN_BRIGHT).monospace());
            let pack_text = state.pack_filter.clone().unwrap_or_else(|| "All".into());
            let prev_filter = state.pack_filter.clone();
            egui::ComboBox::from_id_salt("sampler_pack_combo")
                .width(220.0)
                .selected_text(pack_text)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut state.pack_filter, None, "All");
                    for p in &packs {
                        ui.selectable_value(&mut state.pack_filter, Some(p.clone()), p);
                    }
                });
            // Reflect the change to the user via the status line, so
            // there's no ambiguity about whether the filter took.
            if state.pack_filter != prev_filter {
                state.status = match &state.pack_filter {
                    None => "Pack filter: All".to_string(),
                    Some(p) => format!("Pack filter: {}", p),
                };
            }

            if ui.button("Rescan").clicked() {
                let c = refresh_library(&state.shared);
                state.pack_filter = None;
                state.status = format!("Scanned: {} samples found", c);
            }

            // Push Reset to the right of this row.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Reset")
                    .on_hover_text("Reset Root / Tune / Fine / Trim / Loop / Env / Output to defaults")
                    .clicked()
                {
                    let reset_ids = [
                        P_ROOT, P_TUNE, P_FINE, P_LOOP, P_LOOP_START, P_LOOP_END,
                        P_ATTACK, P_DECAY, P_SUSTAIN, P_RELEASE, P_OUTPUT,
                        P_TRIM_START, P_TRIM_END,
                        P_REVERSE, P_FILTER_TYPE, P_CUTOFF, P_RESO, P_ENV_CUTOFF,
                        P_VEL_AMP, P_VEL_CUTOFF,
                    ];
                    for &pid in &reset_ids {
                        state.shared.params[pid].store(PARAMS[pid].default as f32, Ordering::Relaxed);
                        state.shared.dirty_params[pid].store(true, Ordering::Relaxed);
                    }
                    state.status = "Reset to default values".into();
                }
            });
        });

        // Sample selector — own row with prev/next/Play to its right.
        // Combo gets the rest of the width via expand_to_include_x.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Sample:").color(core_gui::GREEN_BRIGHT).monospace());
            let selected_label = if current_idx >= 0 && (current_idx as usize) < entries.len() {
                let e = &entries[current_idx as usize];
                format!("{} / {}", e.0, e.1)
            } else {
                "(pick one)".into()
            };
            let sample_combo_w = (ui.available_width() - 160.0).max(220.0);
            egui::ComboBox::from_id_salt("sampler_sample_combo")
                .width(sample_combo_w)
                .selected_text(selected_label)
                .show_ui(ui, |ui| {
                    for (i, (pack, stem)) in entries.iter().enumerate() {
                        if let Some(filter) = &state.pack_filter {
                            if pack != filter { continue; }
                        }
                        let label = if state.pack_filter.is_some() {
                            stem.clone()
                        } else {
                            format!("{}  ·  {}", pack, stem)
                        };
                        if ui.selectable_label(current_idx as usize == i, label).clicked() {
                            match pick_sample(&state.shared, i) {
                                Ok(name) => state.status = format!("Loaded: {}", name),
                                Err(e) => state.status = format!("Load failed: {}", e),
                            }
                        }
                    }
                });

            let step_filtered = |delta: i32, state: &mut GuiState| {
                let filter = state.pack_filter.clone();
                let lib = state.shared.library.lock();
                let allowed: Vec<usize> = lib.iter().enumerate()
                    .filter(|(_, e)| filter.as_deref().map_or(true, |f| e.pack == f))
                    .map(|(i, _)| i)
                    .collect();
                drop(lib);
                if allowed.is_empty() { return; }
                let cur = state.shared.current_index.load(Ordering::Relaxed);
                let pos = allowed.iter().position(|&i| i as i32 == cur).unwrap_or(0);
                let new_pos = ((pos as i32 + delta).rem_euclid(allowed.len() as i32)) as usize;
                let new_idx = allowed[new_pos];
                match pick_sample(&state.shared, new_idx) {
                    Ok(name) => state.status = format!("Loaded: {}", name),
                    Err(e) => state.status = format!("Load failed: {}", e),
                }
            };
            if ui.button("◀").on_hover_text("Previous sample (← key)").clicked() {
                step_filtered(-1, state);
            }
            if ui.button("▶").on_hover_text("Next sample (→ key)").clicked() {
                step_filtered(1, state);
            }
            if ui.button("▶ Play")
                .on_hover_text("Audition the current sample (Space)")
                .clicked()
            {
                audition_at_root(&state.shared);
            }
            // Spacebar also auditions while the plugin window is focused.
            if !ui.ctx().wants_keyboard_input()
                && ui.ctx().input(|i| i.key_pressed(egui::Key::Space))
            {
                audition_at_root(&state.shared);
            }
            // Keyboard arrows — work whenever the plugin window is focused,
            // unless a text-edit widget actually wants the key.
            let arrow_left = ctx_input_pressed_arrow(ui.ctx(), egui::Key::ArrowLeft);
            let arrow_right = ctx_input_pressed_arrow(ui.ctx(), egui::Key::ArrowRight);
            if arrow_left { step_filtered(-1, state); }
            if arrow_right { step_filtered(1, state); }
        });

        // Tuner row — shows both the original detected pitch of the
        // sample (read-only) and the pitch it actually plays at when
        // the Root key is auditioned (= original × 2^((Tune+Fine)/12),
        // so the user sees their detuning take effect live).
        let pitch_hz = {
            let g = state.shared.active_sample.lock();
            g.detected_pitch_hz
        };
        let tune_st = state.shared.params[P_TUNE].load(Ordering::Relaxed);
        let fine_ct = state.shared.params[P_FINE].load(Ordering::Relaxed);
        let played_hz = pitch_hz.map(|hz| hz * 2f32.powf((tune_st + fine_ct / 100.0) / 12.0));

        ui.horizontal(|ui| {
            // Native (detected) — what the WAV actually has on disk.
            let (n_name, n_cents) = match pitch_hz {
                Some(hz) => pitch_to_note_name(hz),
                None => ("—".to_string(), 0),
            };
            let native_text = if let Some(hz) = pitch_hz {
                format!("♪ {:<4} {:+4} ct   {:>7.1} Hz", n_name, n_cents, hz)
            } else {
                "♪ —  (no pitch detected)".to_string()
            };
            ui.label(egui::RichText::new("Native:").color(core_gui::GREEN_BRIGHT).monospace());
            ui.label(egui::RichText::new(native_text)
                .color(if pitch_hz.is_some() { core_gui::GREEN } else { core_gui::GREEN_DIM })
                .monospace().strong());

            if let Some(hz) = pitch_hz {
                if ui.button("→ Root")
                    .on_hover_text("Set Root key to the detected note so the sample plays in tune at its native pitch")
                    .clicked()
                {
                    let midi_f = 69.0 + 12.0 * (hz / 440.0).log2();
                    let midi = midi_f.round().clamp(0.0, 127.0);
                    state.shared.params[P_ROOT].store(midi, Ordering::Relaxed);
                    state.shared.dirty_params[P_ROOT].store(true, Ordering::Relaxed);
                    state.shared.params[P_FINE].store(0.0, Ordering::Relaxed);
                    state.shared.dirty_params[P_FINE].store(true, Ordering::Relaxed);
                    state.shared.params[P_TUNE].store(0.0, Ordering::Relaxed);
                    state.shared.dirty_params[P_TUNE].store(true, Ordering::Relaxed);
                    state.status = format!("Root = {} ({:.1} Hz)", n_name, hz);
                }
            }
        });

        // Second tuner row — what the sample sounds like AFTER the
        // current Tune / Fine offsets are applied at the Root key.
        // Highlighted in amber when it differs from native so the
        // user always knows whether they're hearing "as detected" or
        // "with my detune". Cents are vs. nearest semitone of the
        // played pitch, NOT vs. native — that way the user can see
        // both how far the played note sits from a clean semitone
        // and how the offset moved the pitch relative to native.
        ui.horizontal(|ui| {
            let (p_name, p_cents) = match played_hz {
                Some(hz) => pitch_to_note_name(hz),
                None => ("—".to_string(), 0),
            };
            let total_offset_ct = ((tune_st * 100.0) + fine_ct).round() as i32;
            let played_text = if let Some(hz) = played_hz {
                if total_offset_ct == 0 {
                    format!("♪ {:<4} {:+4} ct   {:>7.1} Hz   (unchanged)", p_name, p_cents, hz)
                } else {
                    format!("♪ {:<4} {:+4} ct   {:>7.1} Hz   (offset {:+} ct)",
                            p_name, p_cents, hz, total_offset_ct)
                }
            } else {
                "♪ —".to_string()
            };
            let played_color = if played_hz.is_none() {
                core_gui::GREEN_DIM
            } else if total_offset_ct == 0 {
                core_gui::GREEN_BRIGHT
            } else {
                egui::Color32::from_rgb(220, 170, 60)
            };
            ui.label(egui::RichText::new("Played:").color(core_gui::GREEN_BRIGHT).monospace());
            ui.label(egui::RichText::new(played_text).color(played_color).monospace().strong());
        });

        ui.label(egui::RichText::new(&state.status).color(core_gui::GREEN_DIM).monospace().small());
        // Show source folder of the active sample so it's obvious where the WAV lives.
        let source_hint = {
            let lib = state.shared.library.lock();
            lib.get(current_idx as usize)
                .map(|e| format!("Source: {}", e.path.display()))
                .unwrap_or_else(|| {
                    "Sample folders: ~/Music/SuperDuper Samples/ + ~/Music/Favorite 808s/  (recursive, max depth 4)".into()
                })
        };
        ui.label(egui::RichText::new(source_hint).color(core_gui::GREEN_DIM).monospace().small());

        // Sample-folder management — collapsed by default. Lets the
        // user point the plugin at any folder on disk instead of
        // requiring sample files to live in our hard-coded paths.
        ui.collapsing(
            egui::RichText::new("Sample folders").color(core_gui::GREEN_BRIGHT).monospace(),
            |ui| {
                state.folders_expanded = true;
                let roots = state.shared.sample_roots.lock().clone();
                for (i, root) in roots.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(root.display().to_string())
                            .color(core_gui::GREEN).monospace().small());
                        if ui.small_button("Remove").clicked() {
                            remove_sample_root(&state.shared, i);
                            state.status = format!("Removed: {}", root.display());
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Add:").color(core_gui::GREEN_DIM).monospace());
                    ui.add(
                        egui::TextEdit::singleline(&mut state.folder_input)
                            .desired_width(360.0)
                            .hint_text("/path/to/samples or ~/Music/My 808s"),
                    );
                    if ui.button("+").clicked() {
                        match add_sample_root(&state.shared, &state.folder_input) {
                            Ok(p) => {
                                state.status = format!("Added: {}", p);
                                state.folder_input.clear();
                            }
                            Err(e) => state.status = format!("Add failed: {}", e),
                        }
                    }
                });
                if ui.small_button("Reset to defaults").clicked() {
                    reset_sample_roots(&state.shared);
                    state.status = "Reset to default folders".into();
                }
                ui.label(egui::RichText::new(format!(
                    "Saved to: {}", crate::bank::config_path().display()))
                    .color(core_gui::GREEN_DIM).monospace().small());
            },
        );

        ui.add_space(8.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Pitch", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ROOT], &PARAMS[P_ROOT], &state.shared.dirty_params[P_ROOT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ROOT);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_TUNE], &PARAMS[P_TUNE], &state.shared.dirty_params[P_TUNE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_TUNE);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_FINE], &PARAMS[P_FINE], &state.shared.dirty_params[P_FINE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_FINE);
            });
            core_gui::section(ui, "Trim", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_TRIM_START], &PARAMS[P_TRIM_START], &state.shared.dirty_params[P_TRIM_START], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_TRIM_START);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_TRIM_END], &PARAMS[P_TRIM_END], &state.shared.dirty_params[P_TRIM_END], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_TRIM_END);
            });
            core_gui::section(ui, "Loop", |ui| {
                core_gui::dirty_toggle_row_g(ui, &state.shared.params[P_LOOP], &PARAMS[P_LOOP], &state.shared.dirty_params[P_LOOP], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LOOP);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LOOP_START], &PARAMS[P_LOOP_START], &state.shared.dirty_params[P_LOOP_START], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LOOP_START);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_LOOP_END], &PARAMS[P_LOOP_END], &state.shared.dirty_params[P_LOOP_END], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_LOOP_END);
            });
            core_gui::section(ui, "Envelope", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ATTACK], &PARAMS[P_ATTACK], &state.shared.dirty_params[P_ATTACK], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ATTACK);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_DECAY], &PARAMS[P_DECAY], &state.shared.dirty_params[P_DECAY], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_DECAY);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_SUSTAIN], &PARAMS[P_SUSTAIN], &state.shared.dirty_params[P_SUSTAIN], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_SUSTAIN);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_RELEASE], &PARAMS[P_RELEASE], &state.shared.dirty_params[P_RELEASE], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_RELEASE);
            });
            core_gui::section(ui, "Filter", |ui| {
                // Filter type as a 5-state selector with named options
                // — easier to read than a numeric slider stepping 0..4.
                const FILTER_NAMES: [&str; 5] = ["Off", "LP", "HP", "BP", "Notch"];
                core_gui::dirty_choice_row_g(
                    ui,
                    &state.shared.params[P_FILTER_TYPE],
                    &PARAMS[P_FILTER_TYPE],
                    &FILTER_NAMES,
                    &state.shared.dirty_params[P_FILTER_TYPE],
                    core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end },
                    P_FILTER_TYPE,
                );
                let hz = cutoff_units_to_hz(state.shared.params[P_CUTOFF].load(Ordering::Relaxed));
                let label = if hz < 1000.0 {
                    format!("@ {:.0} Hz", hz)
                } else {
                    format!("@ {:.2} kHz", hz / 1000.0)
                };
                ui.label(egui::RichText::new(label).color(core_gui::GREEN_DIM).monospace().small());
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_CUTOFF], &PARAMS[P_CUTOFF], &state.shared.dirty_params[P_CUTOFF], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_CUTOFF);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_RESO], &PARAMS[P_RESO], &state.shared.dirty_params[P_RESO], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_RESO);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_ENV_CUTOFF], &PARAMS[P_ENV_CUTOFF], &state.shared.dirty_params[P_ENV_CUTOFF], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_ENV_CUTOFF);
            });
            core_gui::section(ui, "Velocity", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_VEL_AMP], &PARAMS[P_VEL_AMP], &state.shared.dirty_params[P_VEL_AMP], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_VEL_AMP);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_VEL_CUTOFF], &PARAMS[P_VEL_CUTOFF], &state.shared.dirty_params[P_VEL_CUTOFF], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_VEL_CUTOFF);
            });
            core_gui::section(ui, "Playback", |ui| {
                core_gui::dirty_toggle_row_g(
                    ui,
                    &state.shared.params[P_REVERSE],
                    &PARAMS[P_REVERSE],
                    &state.shared.dirty_params[P_REVERSE],
                    core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end },
                    P_REVERSE,
                );
            });
            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT], core_gui::GestureBridge { begin: &state.shared.gesture_begin, end: &state.shared.gesture_end }, P_OUTPUT);
            });
            core_gui::help_block(
                ui,
                "sampler_help",
                &[
                    (
                        "Sample folders",
                        "Drop WAV files / packs into ~/Music/SuperDuper Samples/ or \
                         ~/Music/Favorite 808s/. Recursive scan, max depth 4. Each \
                         sub-folder shows up as a Pack in the dropdown. Free WAV \
                         sources: Goldbaby Free Stuff, Hyperreal Music Machines \
                         Roland archive (TR-808/909/606/707/Juno/SH-101).",
                    ),
                    (
                        "Pitch",
                        "Root note is auto-detected by YIN on import (or pick manually). \
                         Tune / Fine offset the pitch in semitones / cents. Playback rate \
                         is computed from `(MIDI note - Root) + Tune + Fine/100`.",
                    ),
                    (
                        "Loop region",
                        "Click + drag the green/orange markers on the waveform to set \
                         Loop Start / Loop End. Loop toggle wraps playback inside the \
                         region after the initial attack. Reverse plays the entire \
                         sample backwards (loop region still respected).",
                    ),
                    (
                        "Filter",
                        "5-mode SVF: Off / LP / HP / BP / Notch. Cutoff in display Hz, \
                         Resonance up to self-oscillation, Env Cutoff modulates from \
                         the amp envelope so a `Vel→Cutoff` swell is one knob away.",
                    ),
                    (
                        "Velocity",
                        "Vel→Amp scales note loudness with MIDI velocity (1 = full, \
                         0 = ignore). Combine with Vel→Cutoff for natural drum dynamics.",
                    ),
                ],
            );
        });
    });
}

/// Draw the main waveform display with trim + loop markers and an
/// ADSR envelope overlay. Handles drag-to-move on the markers.
fn draw_waveform(ui: &mut egui::Ui, state: &mut GuiState, rect: egui::Rect, resp: &egui::Response) {
    let painter = ui.painter_at(rect);
    // Background + frame.
    painter.rect_filled(rect, 3.0, core_gui::PANEL_BG);
    painter.rect_stroke(
        rect, 3.0,
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
        egui::StrokeKind::Inside,
    );
    let centre_y = rect.center().y;
    let half_h = rect.height() * 0.45;

    // Centre zero line.
    painter.line_segment(
        [
            egui::pos2(rect.left(), centre_y),
            egui::pos2(rect.right(), centre_y),
        ],
        egui::Stroke::new(0.5, core_gui::GREEN_FAINT),
    );

    // Read snapshot of marker fracs + ADSR + sample peaks.
    let trim_start = state.shared.params[P_TRIM_START].load(Ordering::Relaxed).clamp(0.0, 1.0);
    let trim_end = state.shared.params[P_TRIM_END].load(Ordering::Relaxed).clamp(0.0, 1.0);
    let loop_on = state.shared.params[P_LOOP].load(Ordering::Relaxed) >= 0.5;
    let loop_start = state.shared.params[P_LOOP_START].load(Ordering::Relaxed).clamp(0.0, 1.0);
    let loop_end = state.shared.params[P_LOOP_END].load(Ordering::Relaxed).clamp(0.0, 1.0);
    let attack = state.shared.params[P_ATTACK].load(Ordering::Relaxed);
    let decay = state.shared.params[P_DECAY].load(Ordering::Relaxed);
    let sustain = state.shared.params[P_SUSTAIN].load(Ordering::Relaxed).clamp(0.0, 1.0);
    let release = state.shared.params[P_RELEASE].load(Ordering::Relaxed);

    // Snapshot peaks under a brief lock — atomic Arc clone keeps us
    // safe even if the audio thread swaps the sample mid-render.
    let sample_arc = {
        let guard = state.shared.active_sample.lock();
        Arc::clone(&*guard)
    };
    let peaks = &sample_arc.peaks;

    // 1. Dim the area OUTSIDE the trim range so it reads "muted".
    let trim_x_lo = lerp_x(rect, trim_start);
    let trim_x_hi = lerp_x(rect, trim_end.max(trim_start));
    let outside_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 110);
    painter.rect_filled(
        egui::Rect::from_min_max(rect.min, egui::pos2(trim_x_lo, rect.max.y)),
        0.0, outside_color,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(trim_x_hi, rect.min.y), rect.max),
        0.0, outside_color,
    );

    // 2. Loop region fill (subtle tint) when loop is engaged and
    //    sits inside the trim window.
    if loop_on {
        let lo = loop_start.max(trim_start);
        let hi = loop_end.min(trim_end).max(lo);
        let lx = lerp_x(rect, lo);
        let rx = lerp_x(rect, hi);
        let loop_color = egui::Color32::from_rgba_unmultiplied(120, 220, 140, 28);
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(lx, rect.min.y), egui::pos2(rx, rect.max.y)),
            0.0, loop_color,
        );
    }

    // 3. The waveform itself — render the peak envelope as a filled
    //    polygon (upper + lower polylines closed at the ends) so the
    //    shape reads as one continuous body, not a row of striped
    //    bars. For each pixel column we collect the min/max across
    //    every bucket that lands in it — that way a 1024-bucket
    //    sample doesn't drop samples at wider widths.
    if !peaks.is_empty() {
        let cols = (rect.width() as usize).clamp(8, 1600);
        let mut upper: Vec<egui::Pos2> = Vec::with_capacity(cols);
        let mut lower: Vec<egui::Pos2> = Vec::with_capacity(cols);
        // Pre-resolve which colour to fill the inside with based on
        // overlap with the trim window — split the polygon at the
        // trim boundaries so the inside-trim portion stays bright.
        let to_y = |v: f32| centre_y - v.clamp(-1.0, 1.0) * half_h;
        let to_x = |frac: f32| rect.left() + frac * rect.width();
        for px in 0..cols {
            let frac_lo = px as f32 / cols as f32;
            let frac_hi = (px + 1) as f32 / cols as f32;
            let b_lo = (frac_lo * peaks.len() as f32) as usize;
            let b_hi = ((frac_hi * peaks.len() as f32).ceil() as usize).min(peaks.len());
            let b_hi = b_hi.max(b_lo + 1);
            let mut mn = f32::INFINITY;
            let mut mx = f32::NEG_INFINITY;
            for &(a, b) in &peaks[b_lo..b_hi] {
                if a < mn { mn = a; }
                if b > mx { mx = b; }
            }
            if !mn.is_finite() { mn = 0.0; }
            if !mx.is_finite() { mx = 0.0; }
            // Guarantee at least 1 px thickness so silent regions
            // still draw a visible centre line instead of vanishing.
            let mx = mx.max(0.003);
            let mn = mn.min(-0.003);
            let x = to_x((frac_lo + frac_hi) * 0.5);
            upper.push(egui::pos2(x, to_y(mx)));
            lower.push(egui::pos2(x, to_y(mn)));
        }
        // Two-pass fill: first dim "outside trim" polygon (whole
        // width with dim colour), then bright "inside trim" polygon
        // clipped to the trim window.
        let dim = egui::Color32::from_rgba_unmultiplied(70, 130, 80, 220);
        let bright = egui::Color32::from_rgba_unmultiplied(120, 220, 140, 240);
        let make_polygon = |upper: &[egui::Pos2], lower: &[egui::Pos2]| -> Vec<egui::Pos2> {
            let mut p = upper.to_vec();
            for q in lower.iter().rev() { p.push(*q); }
            p
        };
        // Dim layer covers the whole sample.
        let full_poly = make_polygon(&upper, &lower);
        painter.add(egui::Shape::convex_polygon(full_poly, dim, egui::Stroke::NONE));
        // Bright overlay just for the trim region — clip by index
        // into upper/lower based on column fraction.
        let trim_lo_col = (trim_start * cols as f32) as usize;
        let trim_hi_col = ((trim_end * cols as f32).ceil() as usize).min(cols);
        if trim_hi_col > trim_lo_col {
            let upper_clip = &upper[trim_lo_col..trim_hi_col];
            let lower_clip = &lower[trim_lo_col..trim_hi_col];
            let bright_poly = make_polygon(upper_clip, lower_clip);
            painter.add(egui::Shape::convex_polygon(bright_poly, bright, egui::Stroke::NONE));
        }
    } else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "( load a sample to see the waveform )",
            egui::FontId::monospace(12.0),
            core_gui::GREEN_DIM,
        );
    }

    // 4. ADSR envelope overlay — drawn from trim-start over the next
    //    "musical" portion of the trim. We compress all four ADSR
    //    stages into a fixed visual fraction (40% of trim) so the
    //    shape stays readable regardless of trim length.
    if !peaks.is_empty() {
        let env_total_frac = 0.4_f32;
        let attack_w = env_total_frac * (attack / (attack + decay + release).max(0.001));
        let decay_w = env_total_frac * (decay / (attack + decay + release).max(0.001));
        let release_w = env_total_frac * (release / (attack + decay + release).max(0.001));
        let env_color = egui::Color32::from_rgba_unmultiplied(255, 200, 80, 220);
        let yfor = |level: f32| centre_y - level.clamp(0.0, 1.0) * half_h;

        // Anchor at trim_start, then attack to 1.0, decay to sustain,
        // sustain plateau (fills the remaining 1 - env_total), release
        // down. Don't go past trim_end.
        let trim_w = (trim_end - trim_start).max(0.0);
        let xat = |f: f32| lerp_x(rect, trim_start + f * trim_w);
        let mut pts: Vec<egui::Pos2> = Vec::with_capacity(6);
        pts.push(egui::pos2(xat(0.0), yfor(0.0)));
        pts.push(egui::pos2(xat(attack_w), yfor(1.0)));
        pts.push(egui::pos2(xat(attack_w + decay_w), yfor(sustain)));
        let plateau_end = (1.0 - release_w).max(attack_w + decay_w);
        pts.push(egui::pos2(xat(plateau_end), yfor(sustain)));
        pts.push(egui::pos2(xat(plateau_end + release_w), yfor(0.0)));
        for w in pts.windows(2) {
            painter.line_segment([w[0], w[1]], egui::Stroke::new(1.5, env_color));
        }
        // ADSR label.
        painter.text(
            egui::pos2(rect.right() - 8.0, rect.top() + 12.0),
            egui::Align2::RIGHT_TOP,
            "ADSR",
            egui::FontId::monospace(10.0),
            env_color,
        );
    }

    // 5. Markers — draw vertical lines for each, plus a clickable
    //    handle box at the bottom. Drag updates the param atomic.
    draw_marker(&painter, rect, trim_start, "S", core_gui::GREEN_BRIGHT);
    draw_marker(&painter, rect, trim_end,   "E", core_gui::GREEN_BRIGHT);
    if loop_on {
        let loop_color = egui::Color32::from_rgb(110, 200, 255);
        draw_marker(&painter, rect, loop_start, "Ls", loop_color);
        draw_marker(&painter, rect, loop_end,   "Le", loop_color);
    }

    // 6. Drag interaction. When the user starts a drag, figure out
    //    which marker is closest and latch onto it. Release ends it.
    handle_marker_drag(state, rect, resp,
        trim_start, trim_end, loop_on, loop_start, loop_end);
}

#[inline]
fn lerp_x(rect: egui::Rect, frac: f32) -> f32 {
    rect.left() + frac.clamp(0.0, 1.0) * rect.width()
}

fn draw_marker(
    painter: &egui::Painter, rect: egui::Rect, frac: f32, label: &str, colour: egui::Color32,
) {
    let x = lerp_x(rect, frac);
    painter.line_segment(
        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        egui::Stroke::new(1.5, colour),
    );
    let handle = egui::Rect::from_min_size(
        egui::pos2(x - 7.0, rect.bottom() - 14.0),
        egui::vec2(14.0, 14.0),
    );
    painter.rect_filled(handle, 2.0, colour);
    painter.text(
        handle.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::monospace(9.0),
        egui::Color32::from_rgb(20, 28, 22),
    );
}

#[allow(clippy::too_many_arguments)]
fn handle_marker_drag(
    state: &mut GuiState,
    rect: egui::Rect,
    resp: &egui::Response,
    trim_start: f32, trim_end: f32,
    loop_on: bool,
    loop_start: f32, loop_end: f32,
) {
    if resp.drag_stopped() {
        state.dragging = None;
    }
    if resp.drag_started() {
        // Pick the closest marker to the press point.
        if let Some(pos) = resp.interact_pointer_pos() {
            let frac_at_pos = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let mut candidates = vec![
                (Marker::TrimStart, (frac_at_pos - trim_start).abs()),
                (Marker::TrimEnd,   (frac_at_pos - trim_end).abs()),
            ];
            if loop_on {
                candidates.push((Marker::LoopStart, (frac_at_pos - loop_start).abs()));
                candidates.push((Marker::LoopEnd,   (frac_at_pos - loop_end).abs()));
            }
            candidates.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            state.dragging = Some(candidates[0].0);
        }
    }
    if resp.dragged() {
        if let (Some(marker), Some(pos)) = (state.dragging, resp.interact_pointer_pos()) {
            let frac = ((pos.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            let (idx, clamped) = match marker {
                Marker::TrimStart => (P_TRIM_START, frac.min(trim_end - 0.001).max(0.0)),
                Marker::TrimEnd   => (P_TRIM_END,   frac.max(trim_start + 0.001).min(1.0)),
                Marker::LoopStart => (P_LOOP_START, frac.min(loop_end - 0.001).max(trim_start)),
                Marker::LoopEnd   => (P_LOOP_END,   frac.max(loop_start + 0.001).min(trim_end)),
            };
            state.shared.params[idx].store(clamped, Ordering::Relaxed);
            state.shared.dirty_params[idx].store(true, Ordering::Relaxed);
        }
    }
}
