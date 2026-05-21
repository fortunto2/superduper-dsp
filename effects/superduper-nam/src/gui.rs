use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{try_load_nam, PARAMS, P_DRIVE, P_INPUT, P_MIX, P_OUTPUT, P_TONE, SharedParams};

fn nam_library_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".superduper-dsp/nam")
}

/// An entry in the library view — path plus a quick "loadable?" check
/// so the GUI can grey-out broken files and skip them in prev/next.
#[derive(Clone)]
pub struct LibraryEntry {
    pub path: std::path::PathBuf,
    /// `None` if the file loads cleanly; `Some(reason)` if loading would
    /// fail (unsupported arch / FiLM / bad JSON / etc.). Computed once at
    /// scan time so we don't re-parse every frame.
    pub unsupported_reason: Option<String>,
    /// Architecture string from the JSON header if it parsed, otherwise
    /// "?". Shown next to the name as a small badge.
    pub arch: String,
}

/// Refresh the list of available `.nam` files in the user's library.
/// Each entry includes a pre-flight check so unsupported models can be
/// marked clearly in the picker without trying to fully load them.
fn scan_library() -> Vec<LibraryEntry> {
    let dir = nam_library_dir();
    let mut out = Vec::new();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(read) = std::fs::read_dir(&dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("nam") {
                continue;
            }
            let (arch, unsupported_reason) = preflight(&path);
            out.push(LibraryEntry {
                path,
                arch,
                unsupported_reason,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Quick header-only validation — reads the JSON, extracts the
/// architecture, and tries to dispatch through `NamModel::from_nam_file`
/// (which catches FiLM / grouped / unknown-arch issues without
/// running inference).
fn preflight(path: &std::path::Path) -> (String, Option<String>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return ("?".into(), Some("read failed".into()));
    };
    let Ok(file) = superduper_synth_core::nam::load_from_json(&text) else {
        return ("?".into(), Some("invalid JSON".into()));
    };
    let arch = file.architecture.clone();
    match superduper_synth_core::nam::NamModel::from_nam_file(&file) {
        Ok(_) => (arch, None),
        Err(e) => (arch, Some(e.to_string())),
    }
}

/// Copy a `.nam` file the user dropped on the plugin window into the
/// library. Returns the new path in the library. If a file with the
/// same name already exists, the new copy is renamed with a `(2)` /
/// `(3)` / … suffix so we never silently overwrite a user's existing
/// model.
fn import_nam_into_library(src: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    if src.extension().and_then(|s| s.to_str()) != Some("nam") {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a .nam file",
        ));
    }
    let dir = nam_library_dir();
    std::fs::create_dir_all(&dir)?;
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("model");
    let mut candidate = dir.join(format!("{stem}.nam"));
    let mut n = 2;
    while candidate.exists() {
        candidate = dir.join(format!("{stem} ({n}).nam"));
        n += 1;
    }
    std::fs::copy(src, &candidate)?;
    Ok(candidate)
}

/// Open the library directory in the OS file manager. Best-effort —
/// failures are silent (the GUI shows the path so the user can navigate
/// manually if `open` is missing).
fn open_library_in_file_manager() {
    let dir = nam_library_dir();
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&dir).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(&dir).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
    }
}

/// Download a `.nam` from a URL into the library directory. Runs on a
/// detached thread because the network call must not block the GUI
/// loop. Sends the resulting `PathBuf` (or error string) back via the
/// `mpsc::Sender` so the GUI can refresh the library when it lands.
fn download_nam_url(
    url: String,
    tx: std::sync::mpsc::Sender<Result<std::path::PathBuf, String>>,
) {
    std::thread::spawn(move || {
        // Pull the filename off the URL. If the URL doesn't end in .nam
        // we still try, but warn the user.
        let stem = url
            .rsplit('/')
            .next()
            .and_then(|s| s.split('?').next())
            .and_then(|s| s.split('#').next())
            .unwrap_or("download")
            .trim_end_matches(".nam");
        let dir = nam_library_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            let _ = tx.send(Err(format!("create dir: {e}")));
            return;
        }
        let mut candidate = dir.join(format!("{stem}.nam"));
        let mut n = 2;
        while candidate.exists() {
            candidate = dir.join(format!("{stem} ({n}).nam"));
            n += 1;
        }
        // We don't bundle an HTTP client to keep the dep tree small;
        // shell out to `curl` (available on all 3 target OSes).
        let status = std::process::Command::new("curl")
            .args(["-sLf", "--max-time", "60", "-o"])
            .arg(&candidate)
            .arg(&url)
            .status();
        match status {
            Ok(s) if s.success() => {
                // Smoke-validate that it parses as JSON to avoid leaving
                // an HTML 404 page sitting in the library.
                let bytes = std::fs::read_to_string(&candidate).unwrap_or_default();
                if !bytes.trim_start().starts_with('{') {
                    let _ = std::fs::remove_file(&candidate);
                    let _ = tx.send(Err(format!("URL did not return JSON: {url}")));
                    return;
                }
                let _ = tx.send(Ok(candidate));
            }
            Ok(s) => {
                let _ = std::fs::remove_file(&candidate);
                let _ = tx.send(Err(format!("curl exit code {}", s.code().unwrap_or(-1))));
            }
            Err(e) => {
                let _ = tx.send(Err(format!("curl spawn failed: {e}")));
            }
        }
    });
}

pub const DEFAULT_WIDTH: u32 = 580;
pub const DEFAULT_HEIGHT: u32 = 460;
pub const MIN_WIDTH: u32 = 460;
pub const MIN_HEIGHT: u32 = 360;
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
    library: Vec<LibraryEntry>,
    last_load_error: Option<String>,
    pending_delete: Option<std::path::PathBuf>,
    url_input: String,
    download_in_flight: Option<String>,
    download_rx: std::sync::mpsc::Receiver<Result<std::path::PathBuf, String>>,
    download_tx: std::sync::mpsc::Sender<Result<std::path::PathBuf, String>>,
    /// Substring filter — only library entries whose name contains
    /// this (case-insensitive) show up in the picker.
    filter: String,
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P,
    shared: SharedParams,
    resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper NAM".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let preset_idx =
        (shared.active_preset.load(Ordering::Relaxed) as usize).min(PRESETS.len().saturating_sub(1));
    let (download_tx, download_rx) = std::sync::mpsc::channel();
    let state = GuiState {
        shared,
        resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(preset_idx),
        preset_names: PRESETS.iter().map(|p| p.name).collect(),
        library: scan_library(),
        last_load_error: None,
        pending_delete: None,
        url_input: String::new(),
        download_in_flight: None,
        download_rx,
        download_tx,
        filter: String::new(),
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
            "SuperDuper NAM",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "nam_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            if let Some(preset) = PRESETS.get(i) {
                crate::presets::apply(&state.shared, preset);
                state.shared.active_preset.store(i as u32, Ordering::Relaxed);
            }
        }

        core_gui::ab_init_bar(
            ui,
            &state.shared.ab_snapshot,
            &state.shared.params,
            PARAMS,
            &state.shared.dirty_params,
        );

        // ============= File-drop accept zone ==============
        // egui exposes raw dropped-file events on the context. We accept
        // any .nam path the user drags from Finder / a browser onto the
        // plugin window and copy it into the library.
        let dropped: Vec<egui::DroppedFile> = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            if let Some(path) = f.path {
                match import_nam_into_library(&path) {
                    Ok(dest) => {
                        state.library = scan_library();
                        // Auto-load whatever the user dropped — most users
                        // want immediate audio feedback.
                        match try_load_nam(&dest) {
                            Ok((net, name)) => {
                                *state.shared.pending_net.lock() = Some(net);
                                *state.shared.current_model_name.lock() = name;
                                state.last_load_error = None;
                            }
                            Err(e) => state.last_load_error = Some(e.to_string()),
                        }
                    }
                    Err(e) => state.last_load_error = Some(format!("import: {e}")),
                }
            }
        }
        // Show hover feedback while files are mid-drag.
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            ui.label(
                egui::RichText::new("DROP .nam FILES HERE")
                    .color(core_gui::GREEN_BRIGHT)
                    .monospace()
                    .strong(),
            );
        }

        // ============= Pending downloads pickup ==============
        if let Ok(result) = state.download_rx.try_recv() {
            state.download_in_flight = None;
            match result {
                Ok(path) => {
                    state.library = scan_library();
                    if let Ok((net, name)) = try_load_nam(&path) {
                        *state.shared.pending_net.lock() = Some(net);
                        *state.shared.current_model_name.lock() = name;
                    }
                    state.last_load_error = None;
                }
                Err(e) => state.last_load_error = Some(e),
            }
        }

        // ============= Filtered library view (used by both header arrows
        // and the list below). Filter is case-insensitive substring match.
        // Prev/next arrows also skip entries we already know are
        // unsupported — no point landing on a model that will only
        // re-display the same error.
        let filter_lower = state.filter.to_lowercase();
        let filtered_indices: Vec<usize> = state
            .library
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let name = e
                    .path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if filter_lower.is_empty() || name.contains(&filter_lower) {
                    Some(i)
                } else {
                    None
                }
            })
            .collect();
        // For arrow navigation: same as filtered, but skip unsupported.
        let nav_indices: Vec<usize> = filtered_indices
            .iter()
            .copied()
            .filter(|&i| {
                state
                    .library
                    .get(i)
                    .map(|e| e.unsupported_reason.is_none())
                    .unwrap_or(false)
            })
            .collect();

        // Find current model's position in the navigation view.
        let current_name = state.shared.current_model_name.lock().clone();
        let current_idx_in_nav = nav_indices.iter().position(|&i| {
            state
                .library
                .get(i)
                .map(|e| {
                    e.path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s == current_name)
                        .unwrap_or(false)
                })
                .unwrap_or(false)
        });

        // Load helper closure — invoked by arrow buttons + list clicks.
        let load_by_lib_index = |state: &mut GuiState, lib_idx: usize| {
            if let Some(entry) = state.library.get(lib_idx).cloned() {
                match try_load_nam(&entry.path) {
                    Ok((net, name)) => {
                        *state.shared.pending_net.lock() = Some(net);
                        *state.shared.current_model_name.lock() = name;
                        state.last_load_error = None;
                    }
                    Err(e) => {
                        state.last_load_error =
                            Some(format!("{}: {}", entry.path.display(), e));
                    }
                }
            }
        };

        // ============= Model header: name + prev/next + position indicator
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("model: {}", current_name))
                    .color(core_gui::GREEN_BRIGHT)
                    .monospace(),
            );
            ui.add_space(8.0);
            // Prev/Next walks through *navigable* entries only
            // (filtered + supported), wraps around. Unsupported files
            // remain visible in the list but you can't accidentally
            // arrow into them.
            let total = nav_indices.len();
            let enabled = total > 0;
            if enabled
                && ui
                    .selectable_label(
                        false,
                        egui::RichText::new("[<]").color(core_gui::GREEN).monospace(),
                    )
                    .on_hover_text("previous loadable model")
                    .clicked()
            {
                let pos = current_idx_in_nav.unwrap_or(0);
                let prev = if pos == 0 { total - 1 } else { pos - 1 };
                load_by_lib_index(state, nav_indices[prev]);
            }
            let pos_text = match current_idx_in_nav {
                Some(p) => format!("[{}/{}]", p + 1, total),
                None if total > 0 => format!("[-/{}]", total),
                None => "[0/0]".to_string(),
            };
            ui.label(
                egui::RichText::new(pos_text)
                    .color(core_gui::GREEN_DIM)
                    .monospace(),
            );
            if enabled
                && ui
                    .selectable_label(
                        false,
                        egui::RichText::new("[>]").color(core_gui::GREEN).monospace(),
                    )
                    .on_hover_text("next loadable model")
                    .clicked()
            {
                let pos = current_idx_in_nav.unwrap_or(usize::MAX);
                let next = if pos == usize::MAX || pos + 1 >= total {
                    0
                } else {
                    pos + 1
                };
                load_by_lib_index(state, nav_indices[next]);
            }
            ui.add_space(8.0);
            if ui
                .selectable_label(
                    false,
                    egui::RichText::new("[reload]").color(core_gui::GREEN).monospace(),
                )
                .clicked()
            {
                state.library = scan_library();
            }
            if ui
                .selectable_label(
                    false,
                    egui::RichText::new("[open folder]")
                        .color(core_gui::GREEN)
                        .monospace(),
                )
                .clicked()
            {
                open_library_in_file_manager();
            }
        });

        // ============= URL paste row ==============
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("url:")
                    .color(core_gui::GREEN_DIM)
                    .monospace(),
            );
            let resp = ui.add(
                egui::TextEdit::singleline(&mut state.url_input)
                    .desired_width(330.0)
                    .hint_text("paste direct .nam URL"),
            );
            let submitted = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let clicked = ui
                .selectable_label(
                    false,
                    egui::RichText::new("[download]")
                        .color(core_gui::GREEN)
                        .monospace(),
                )
                .clicked();
            if (submitted || clicked)
                && !state.url_input.is_empty()
                && state.download_in_flight.is_none()
            {
                let url = state.url_input.trim().to_string();
                state.download_in_flight = Some(url.clone());
                download_nam_url(url, state.download_tx.clone());
                state.url_input.clear();
            }
            if let Some(ref url) = state.download_in_flight {
                ui.label(
                    egui::RichText::new(format!("downloading: {}", url))
                        .color(core_gui::GREEN_BRIGHT)
                        .monospace()
                        .small(),
                );
            }
        });

        // ============= Library list with filter + scroll + per-row delete
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("library:")
                    .color(core_gui::GREEN_DIM)
                    .monospace(),
            );
            ui.add(
                egui::TextEdit::singleline(&mut state.filter)
                    .desired_width(180.0)
                    .hint_text("filter…"),
            );
            if !state.filter.is_empty()
                && ui
                    .selectable_label(
                        false,
                        egui::RichText::new("[x]").color(core_gui::GREEN_DIM).monospace(),
                    )
                    .on_hover_text("clear filter")
                    .clicked()
            {
                state.filter.clear();
            }
            ui.label(
                egui::RichText::new(format!("{} of {}", filtered_indices.len(), state.library.len()))
                    .color(core_gui::GREEN_DIM)
                    .monospace()
                    .small(),
            );
        });
        if state.library.is_empty() {
            ui.label(
                egui::RichText::new(
                    "(empty — drag .nam files here, or drop them in ~/.superduper-dsp/nam/)",
                )
                .color(core_gui::GREEN_DIM)
                .monospace()
                .small(),
            );
        } else if filtered_indices.is_empty() {
            ui.label(
                egui::RichText::new(format!("(no matches for `{}`)", state.filter))
                    .color(core_gui::GREEN_DIM)
                    .monospace()
                    .small(),
            );
        }
        // Scrollable area — grows to ~160 px tall, scrolls inside if more.
        egui::ScrollArea::vertical()
            .id_salt("nam_library_scroll")
            .max_height(160.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let entries: Vec<LibraryEntry> = filtered_indices
                    .iter()
                    .filter_map(|&i| state.library.get(i).cloned())
                    .collect();
                for entry in entries.iter() {
                    let label = entry
                        .path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("?")
                        .to_string();
                    let selected = current_name == label;
                    let is_supported = entry.unsupported_reason.is_none();
                    let name_colour = if is_supported {
                        core_gui::GREEN
                    } else {
                        core_gui::GREEN_DIM
                    };
                    ui.horizontal(|ui| {
                        // Architecture badge — small dim tag in front of
                        // the name so the user knows what they're picking.
                        ui.label(
                            egui::RichText::new(format!("{:7}", entry.arch))
                                .color(core_gui::GREEN_DIM)
                                .monospace()
                                .small(),
                        );
                        let resp = ui.selectable_label(
                            selected,
                            egui::RichText::new(&label).color(name_colour).monospace(),
                        );
                        if let Some(reason) = entry.unsupported_reason.as_ref() {
                            // Hover tooltip explains why it's unsupported.
                            resp.clone().on_hover_text(format!(
                                "unsupported: {}\n(file kept on disk — won't load)",
                                reason
                            ));
                            ui.label(
                                egui::RichText::new("(unsupported)")
                                    .color(egui::Color32::from_rgb(230, 160, 90))
                                    .monospace()
                                    .small(),
                            );
                        } else if resp.clicked() {
                            match try_load_nam(&entry.path) {
                                Ok((net, name)) => {
                                    *state.shared.pending_net.lock() = Some(net);
                                    *state.shared.current_model_name.lock() = name;
                                    state.last_load_error = None;
                                }
                                Err(e) => {
                                    state.last_load_error =
                                        Some(format!("{}: {}", entry.path.display(), e));
                                }
                            }
                        }
                        if ui
                            .selectable_label(
                                false,
                                egui::RichText::new("[×]")
                                    .color(egui::Color32::from_rgb(230, 100, 90))
                                    .monospace(),
                            )
                            .on_hover_text("delete .nam from library")
                            .clicked()
                        {
                            state.pending_delete = Some(entry.path.clone());
                        }
                    });
                }
            });
        // ============= Delete confirm prompt ==============
        if let Some(ref del_path) = state.pending_delete.clone() {
            ui.horizontal(|ui| {
                let label = del_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?");
                ui.label(
                    egui::RichText::new(format!("delete {}?", label))
                        .color(egui::Color32::from_rgb(230, 100, 90))
                        .monospace(),
                );
                if ui
                    .selectable_label(
                        false,
                        egui::RichText::new("[yes]").color(core_gui::GREEN).monospace(),
                    )
                    .clicked()
                {
                    if let Err(e) = std::fs::remove_file(del_path) {
                        state.last_load_error = Some(format!("delete: {e}"));
                    } else {
                        state.library = scan_library();
                    }
                    state.pending_delete = None;
                }
                if ui
                    .selectable_label(
                        false,
                        egui::RichText::new("[no]").color(core_gui::GREEN_DIM).monospace(),
                    )
                    .clicked()
                {
                    state.pending_delete = None;
                }
            });
        }
        if let Some(ref err) = state.last_load_error {
            ui.label(
                egui::RichText::new(format!("error: {}", err))
                    .color(egui::Color32::from_rgb(230, 100, 90))
                    .monospace()
                    .small(),
            );
        }

        let (scope_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 80.0),
            egui::Sense::hover(),
        );
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, scope_rect, 48_000.0);

        ui.add_space(4.0);

        let g = || core_gui::GestureBridge {
            begin: &state.shared.gesture_begin,
            end: &state.shared.gesture_end,
        };
        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Network", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_INPUT], &PARAMS[P_INPUT], &state.shared.dirty_params[P_INPUT], g(), P_INPUT);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_DRIVE], &PARAMS[P_DRIVE], &state.shared.dirty_params[P_DRIVE], g(), P_DRIVE);
            });
            core_gui::section(ui, "Tone", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_TONE], &PARAMS[P_TONE], &state.shared.dirty_params[P_TONE], g(), P_TONE);
            });
            core_gui::section(ui, "Output", |ui| {
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_OUTPUT], &PARAMS[P_OUTPUT], &state.shared.dirty_params[P_OUTPUT], g(), P_OUTPUT);
                core_gui::dirty_param_row_g(ui, &state.shared.params[P_MIX], &PARAMS[P_MIX], &state.shared.dirty_params[P_MIX], g(), P_MIX);
            });
            ui.add_space(8.0);
            core_gui::help_block_with_links(
                ui,
                "nam_help",
                &[
                    (
                        "What is NAM?",
                        "Neural Amp Modeler — neural-network emulation of guitar amps, \
                         tube preamps, and stompboxes. A `.nam` file contains the \
                         trained weights of a small WaveNet or LSTM. Drop one on the \
                         window or paste its URL — the plugin loads it on the fly.",
                        &[],
                    ),
                    (
                        "Where to find models",
                        "ToneHunt is the biggest library (free account required to \
                         download; once logged in, drag the .nam file off the page). \
                         Tone3000 hosts NAM + AIDA-X models with direct links. NAM \
                         Hub aggregates curator picks. GitHub example_models has the \
                         reference models from sdatkinson.",
                        &[
                            ("ToneHunt", "https://tonehunt.org"),
                            ("Tone3000", "https://tone3000.com"),
                            ("NAM Hub", "https://nam.parametric.audio"),
                            (
                                "GitHub examples",
                                "https://github.com/sdatkinson/NeuralAmpModelerCore/tree/main/example_models",
                            ),
                        ],
                    ),
                    (
                        "How to load",
                        "1) Drag .nam from the browser/Finder onto this window. \
                         2) Paste a direct .nam URL into the [url] field. \
                         3) Drop files into ~/.superduper-dsp/nam/ and hit [reload]. \
                         All three update the library list below. Clicking a name \
                         switches the model — Input/Drive scale into the network, \
                         Output/Tone shape the result.",
                        &[],
                    ),
                    (
                        "Supported architectures",
                        "WaveNet: Standard, Lite, Nano, with optional gated/blended \
                         activation and head1x1. LSTM: any number of layers, any \
                         hidden size. FiLM modulation and grouped convolutions are \
                         not supported (no community models use them in practice).",
                        &[],
                    ),
                    (
                        "Tools",
                        "Run `cargo run --release -p nam-test` from the repo to \
                         smoke-test every file in your library — silence / DC / 1 kHz \
                         sine / 50 Hz→8 kHz sweep probes, with finite + RMS checks.",
                        &[],
                    ),
                ],
            );
        });
    });
}
