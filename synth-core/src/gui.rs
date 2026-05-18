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

// ---------------------------------------------------------------------------
// MIDI Learn — bind any MIDI CC to any plugin param at runtime via a
// right-click context menu, instead of needing a hard-coded CC table.
//
// Storage: per-plugin `MidiLearnState` sitting on `Shared`.  GUI
// thread arms a learn by storing the target param index into `pending`;
// the audio thread's `handle_cc()` consumes the next CC, stores the
// mapping, and clears `pending`.  Subsequent CC events look up the
// mapping table and write directly into the param atomic.
// ---------------------------------------------------------------------------

pub struct MidiLearnState {
    /// CC number 0..=127 → param index.
    pub mappings: parking_lot::Mutex<std::collections::HashMap<u8, usize>>,
    /// Currently-pending learn target.  `-1` = idle; `>=0` = arm: next CC
    /// will bind to that param index.
    pub pending: std::sync::atomic::AtomicI32,
}

impl Default for MidiLearnState {
    fn default() -> Self {
        Self::new()
    }
}

impl MidiLearnState {
    pub fn new() -> Self {
        Self {
            mappings: parking_lot::Mutex::new(std::collections::HashMap::new()),
            pending: std::sync::atomic::AtomicI32::new(-1),
        }
    }
    /// Process an incoming CC event. Returns:
    /// - `Some(idx)` if this CC is bound to a param — caller writes the
    ///   normalised value into that param atomic.
    /// - `None` if the CC was consumed by a pending Learn (don't apply
    ///   the value), OR if the CC has no mapping (caller may fall back
    ///   to a hardcoded mapping table or ignore it).
    pub fn handle_cc(&self, cc: u8) -> Option<usize> {
        let pending = self.pending.load(Ordering::Relaxed);
        if pending >= 0 {
            self.mappings.lock().insert(cc, pending as usize);
            self.pending.store(-1, Ordering::Relaxed);
            return None;
        }
        self.mappings.lock().get(&cc).copied()
    }
    pub fn arm(&self, param_idx: usize) {
        self.pending.store(param_idx as i32, Ordering::Relaxed);
    }
    pub fn cancel(&self) {
        self.pending.store(-1, Ordering::Relaxed);
    }
    pub fn clear_for_param(&self, param_idx: usize) {
        self.mappings.lock().retain(|_, v| *v != param_idx);
    }
    pub fn is_learning(&self, param_idx: usize) -> bool {
        self.pending.load(Ordering::Relaxed) == param_idx as i32
    }
    /// Snapshot the mappings as a (cc, param_idx) pair vec — for state
    /// serialisation and Restore.
    pub fn snapshot(&self) -> Vec<(u8, usize)> {
        let m = self.mappings.lock();
        let mut out: Vec<(u8, usize)> = m.iter().map(|(&k, &v)| (k, v)).collect();
        out.sort_by_key(|(cc, _)| *cc);
        out
    }
    pub fn replace(&self, pairs: &[(u8, usize)]) {
        let mut m = self.mappings.lock();
        m.clear();
        for &(cc, idx) in pairs {
            m.insert(cc, idx);
        }
    }
}

/// Param row with right-click "MIDI Learn" + "Clear MIDI" entries.
/// Same drag behaviour as `dirty_param_row`; the only difference is the
/// context menu and a tiny status badge if this param is currently
/// armed for Learn.
pub fn learn_param_row(
    ui: &mut egui::Ui,
    atom: &AtomicF32,
    def: &ParamDef,
    dirty: &AtomicBool,
    learn: &MidiLearnState,
    param_idx: usize,
) {
    let before = atom.load(Ordering::Relaxed);
    let name = std::str::from_utf8(def.name)
        .unwrap_or("?")
        .trim_end_matches('\0');
    let learning = learn.is_learning(param_idx);
    let bound_cc = {
        let m = learn.mappings.lock();
        m.iter().find_map(|(&cc, &idx)| (idx == param_idx).then_some(cc))
    };

    let label_text = if learning {
        format!("{name}  · LEARN")
    } else if let Some(cc) = bound_cc {
        format!("{name}  · CC{cc}")
    } else {
        name.to_string()
    };

    let response = ui.horizontal(|ui| {
        let colour = if learning { GREEN_BRIGHT } else { GREEN };
        let label_resp = ui.add_sized(
            [120.0, 18.0],
            egui::Label::new(
                egui::RichText::new(&label_text).color(colour).monospace(),
            )
            .sense(egui::Sense::click()),
        );
        let mut value = before;
        let slider = egui::Slider::new(&mut value, (def.min as f32)..=(def.max as f32))
            .show_value(true)
            .clamping(egui::SliderClamping::Always)
            .suffix(if def.unit.is_empty() {
                String::new()
            } else {
                format!(" {}", def.unit)
            });
        let slider_resp = ui.add(slider);
        if slider_resp.changed() {
            atom.store(value, Ordering::Relaxed);
            dirty.store(true, Ordering::Relaxed);
        }
        label_resp.union(slider_resp)
    });
    response.inner.context_menu(|ui| {
        if ui.button("MIDI Learn (assign next CC)").clicked() {
            learn.arm(param_idx);
            ui.close_menu();
        }
        if bound_cc.is_some() {
            if ui.button("Clear MIDI mapping").clicked() {
                learn.clear_for_param(param_idx);
                ui.close_menu();
            }
        }
        if learning {
            if ui.button("Cancel learn").clicked() {
                learn.cancel();
                ui.close_menu();
            }
        }
    });
}

/// Same as `learn_param_row` but also fires CLAP gesture begin/end events.
/// Use in plugins that opted-in to gesture reporting — Wave / Kubyz so far.
pub fn learn_param_row_g(
    ui: &mut egui::Ui,
    atom: &AtomicF32,
    def: &ParamDef,
    dirty: &AtomicBool,
    learn: &MidiLearnState,
    param_idx: usize,
    gesture: GestureBridge<'_>,
) {
    let name = std::str::from_utf8(def.name)
        .unwrap_or("?")
        .trim_end_matches('\0');
    let learning = learn.is_learning(param_idx);
    let bound_cc = {
        let m = learn.mappings.lock();
        m.iter().find_map(|(&cc, &idx)| (idx == param_idx).then_some(cc))
    };

    let label_text = if learning {
        format!("{name}  · LEARN")
    } else if let Some(cc) = bound_cc {
        format!("{name}  · CC{cc}")
    } else {
        name.to_string()
    };

    let response = ui.horizontal(|ui| {
        let colour = if learning { GREEN_BRIGHT } else { GREEN };
        let label_resp = ui.add_sized(
            [120.0, 18.0],
            egui::Label::new(
                egui::RichText::new(&label_text).color(colour).monospace(),
            )
            .sense(egui::Sense::click()),
        );
        let mut value = atom.load(Ordering::Relaxed);
        let slider = egui::Slider::new(&mut value, (def.min as f32)..=(def.max as f32))
            .show_value(true)
            .clamping(egui::SliderClamping::Always)
            .suffix(if def.unit.is_empty() {
                String::new()
            } else {
                format!(" {}", def.unit)
            });
        let slider_resp = ui.add(slider);
        if slider_resp.drag_started() {
            if let Some(b) = gesture.begin.get(param_idx) {
                b.store(true, Ordering::Relaxed);
            }
        }
        if slider_resp.changed() {
            atom.store(value, Ordering::Relaxed);
            dirty.store(true, Ordering::Relaxed);
        }
        if slider_resp.drag_stopped() {
            if let Some(e) = gesture.end.get(param_idx) {
                e.store(true, Ordering::Relaxed);
            }
        }
        label_resp.union(slider_resp)
    });
    response.inner.context_menu(|ui| {
        if ui.button("MIDI Learn (assign next CC)").clicked() {
            learn.arm(param_idx);
            ui.close_menu();
        }
        if bound_cc.is_some() {
            if ui.button("Clear MIDI mapping").clicked() {
                learn.clear_for_param(param_idx);
                ui.close_menu();
            }
        }
        if learning {
            if ui.button("Cancel learn").clicked() {
                learn.cancel();
                ui.close_menu();
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Live oscilloscope — a tiny lock-free ring buffer for the audio thread
// to deposit recent samples in, and the GUI to read for visualisation.
// Per-slot atomic so we never need a Mutex: audio thread bumps `head`
// and stores into the slot; GUI walks backwards from head reading the
// snapshot. Inconsistencies under load look like a slight wiggle, not
// like dropouts — perfectly acceptable for a meter.
// ---------------------------------------------------------------------------

pub struct LiveScope {
    pub buf: Box<[AtomicF32]>,
    pub head: std::sync::atomic::AtomicUsize,
}

impl LiveScope {
    pub fn new(capacity: usize) -> Self {
        let buf: Vec<AtomicF32> = (0..capacity).map(|_| AtomicF32::new(0.0)).collect();
        Self {
            buf: buf.into_boxed_slice(),
            head: std::sync::atomic::AtomicUsize::new(0),
        }
    }
    /// Audio-thread: push one sample (mono). Lock-free.
    #[inline]
    pub fn push(&self, x: f32) {
        let cap = self.buf.len();
        if cap == 0 {
            return;
        }
        let h = self.head.fetch_add(1, Ordering::Relaxed) % cap;
        self.buf[h].store(x, Ordering::Relaxed);
    }
    /// GUI-thread: copy the last `out.len()` samples in chronological order.
    pub fn snapshot(&self, out: &mut [f32]) {
        let cap = self.buf.len();
        if cap == 0 || out.is_empty() {
            return;
        }
        let head = self.head.load(Ordering::Relaxed);
        let start = head.wrapping_sub(out.len());
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = self.buf[start.wrapping_add(i) % cap].load(Ordering::Relaxed);
        }
    }
}

/// Render the live scope inside `rect` — green polyline + centre line.
pub fn draw_scope(ui: &mut egui::Ui, scope: &LiveScope, rect: egui::Rect, samples: usize) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, PANEL_BG);
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, GREEN_FAINT),
        egui::epaint::StrokeKind::Outside,
    );
    let centre_y = rect.center().y;
    painter.line_segment(
        [egui::pos2(rect.left(), centre_y), egui::pos2(rect.right(), centre_y)],
        egui::Stroke::new(0.5, GREEN_FAINT),
    );
    let mut buf = vec![0.0_f32; samples];
    scope.snapshot(&mut buf);
    let half_h = rect.height() * 0.45;
    let step_x = rect.width() / (samples.saturating_sub(1).max(1) as f32);
    let mut prev: Option<egui::Pos2> = None;
    for (i, v) in buf.iter().copied().enumerate() {
        let x = rect.left() + i as f32 * step_x;
        let y = centre_y - v.clamp(-1.5, 1.5) * half_h;
        let pt = egui::pos2(x, y);
        if let Some(p) = prev {
            painter.line_segment([p, pt], egui::Stroke::new(1.0, GREEN_BRIGHT));
        }
        prev = Some(pt);
    }
}

/// Live magnitude-spectrum strip — log-frequency X-axis, dB Y-axis.
/// Far more informative on synths than the raw waveform: you see exactly
/// which harmonics are loud, where the filter is, and how unison spread
/// pulls the partials into clusters.
pub fn draw_spectrum_strip(
    ui: &mut egui::Ui,
    scope: &LiveScope,
    rect: egui::Rect,
    sr: f32,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, PANEL_BG);
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, GREEN_FAINT),
        egui::epaint::StrokeKind::Outside,
    );
    // FFT size — must be a power of two; 1024 covers 20 Hz – 20 kHz with
    // ~47 Hz bin width which is more than enough for a visualiser.
    const FFT_N: usize = 1024;
    let mut buf = vec![0.0_f32; FFT_N];
    scope.snapshot(&mut buf);
    // Skip the FFT cost when the signal is silent (zero RMS) — saves
    // 60 Hz × 1024-point FFTs on inactive plugins.
    let rms: f32 = (buf.iter().map(|x| x * x).sum::<f32>() / FFT_N as f32).sqrt();
    if rms < 1e-5 {
        // Just paint a dim baseline and bail.
        let y = rect.bottom() - 4.0;
        painter.line_segment(
            [egui::pos2(rect.left() + 2.0, y), egui::pos2(rect.right() - 2.0, y)],
            egui::Stroke::new(1.0, GREEN_FAINT),
        );
        return;
    }
    let mag_db = crate::analysis::magnitude_spectrum_db(&buf);

    // X axis maps log frequency (20 Hz .. 20 kHz) onto rect.left..right.
    // Y axis maps -80 dB .. 0 dB onto rect.bottom..top.
    let f_min = 20.0_f32;
    let f_max = 20_000.0_f32.min(sr * 0.5);
    let db_min = -80.0_f32;
    let db_max = 0.0_f32;
    let to_x = |freq: f32| {
        let f = freq.max(f_min);
        rect.left()
            + ((f.ln() - f_min.ln()) / (f_max.ln() - f_min.ln())) * rect.width()
    };
    let to_y = |db: f32| {
        let db = db.clamp(db_min, db_max);
        rect.bottom() - ((db - db_min) / (db_max - db_min)) * rect.height()
    };

    // Faint grid — log-decade lines at 100 / 1000 / 10000 Hz.
    for &g in &[100.0_f32, 1000.0, 10000.0] {
        let x = to_x(g);
        if x > rect.left() + 1.0 && x < rect.right() - 1.0 {
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(0.5, GREEN_FAINT),
            );
        }
    }
    // -40 dB line for reference.
    let mid_y = to_y(-40.0);
    painter.line_segment(
        [egui::pos2(rect.left(), mid_y), egui::pos2(rect.right(), mid_y)],
        egui::Stroke::new(0.5, GREEN_FAINT),
    );

    // Polyline over the bins (skip DC bin 0 — log scale doesn't reach 0).
    let n_bins = mag_db.len();
    let mut prev: Option<egui::Pos2> = None;
    for bin in 1..n_bins {
        let freq = bin as f32 * sr / (FFT_N as f32);
        if freq < f_min {
            continue;
        }
        if freq > f_max {
            break;
        }
        let db = mag_db[bin];
        let pt = egui::pos2(to_x(freq), to_y(db));
        if let Some(p) = prev {
            painter.line_segment([p, pt], egui::Stroke::new(1.0, GREEN_BRIGHT));
        }
        prev = Some(pt);
    }
}

// ---------------------------------------------------------------------------
// User file-presets — save/load arbitrary parameter snapshots to
// ~/.superduper-dsp/<plugin>/presets/<name>.json. Plugins call into
// these helpers from their GUI; the JSON format is intentionally simple
// (just the param vector + optional extra section for plugin-specific
// blobs the simple_state helper can't serialise alone).
// ---------------------------------------------------------------------------

/// Folder where a plugin keeps its user presets. Auto-created on first save.
pub fn user_preset_dir(plugin_slug: &str) -> std::path::PathBuf {
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    home.join(".superduper-dsp")
        .join(plugin_slug)
        .join("presets")
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct UserPreset {
    pub version: u32,
    pub name: String,
    pub params: Vec<f32>,
    /// Optional plugin-specific blob (Wave's frame_a curve, Kubyz harmonics,
    /// etc) — serialised as opaque JSON so each plugin can stash its own
    /// shape without breaking the shared loader.
    #[serde(default)]
    pub extra: serde_json::Value,
}

pub const USER_PRESET_VERSION: u32 = 1;

pub fn save_user_preset(
    plugin_slug: &str,
    name: &str,
    params: &[AtomicF32],
    extra: serde_json::Value,
) -> std::io::Result<std::path::PathBuf> {
    let dir = user_preset_dir(plugin_slug);
    std::fs::create_dir_all(&dir)?;
    // Sanitise the filename — keep ASCII alphanum + `-` + `_` + spaces.
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_') { c } else { '_' }
        })
        .collect();
    let trimmed = safe.trim();
    let final_name = if trimmed.is_empty() { "preset" } else { trimmed };
    let path = dir.join(format!("{final_name}.json"));
    let preset = UserPreset {
        version: USER_PRESET_VERSION,
        name: final_name.to_string(),
        params: params.iter().map(|a| a.load(Ordering::Relaxed)).collect(),
        extra,
    };
    let file = std::fs::File::create(&path)?;
    serde_json::to_writer_pretty(file, &preset)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    Ok(path)
}

pub fn list_user_presets(plugin_slug: &str) -> Vec<std::path::PathBuf> {
    let dir = user_preset_dir(plugin_slug);
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) == Some("json") {
            out.push(p);
        }
    }
    out.sort();
    out
}

pub fn load_user_preset(
    path: &std::path::Path,
    params: &[AtomicF32],
    dirty: &[AtomicBool],
) -> std::io::Result<UserPreset> {
    let file = std::fs::File::open(path)?;
    let preset: UserPreset = serde_json::from_reader(file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    if preset.version != USER_PRESET_VERSION {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "preset version mismatch",
        ));
    }
    for (i, v) in preset.params.iter().enumerate() {
        if let Some(atom) = params.get(i) {
            atom.store(*v, Ordering::Relaxed);
            if let Some(d) = dirty.get(i) {
                d.store(true, Ordering::Relaxed);
            }
        }
    }
    Ok(preset)
}

/// A/B snapshot pair — each slot is a frozen copy of every param.
/// `current` points at which slot the user is editing right now.
#[derive(Default)]
pub struct AbSnapshot {
    pub a: std::sync::Mutex<Vec<f32>>,
    pub b: std::sync::Mutex<Vec<f32>>,
    pub current_is_b: std::sync::atomic::AtomicBool,
}

impl AbSnapshot {
    pub fn new(n: usize) -> Self {
        Self {
            a: std::sync::Mutex::new(vec![0.0; n]),
            b: std::sync::Mutex::new(vec![0.0; n]),
            current_is_b: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

/// Snapshot the live params into the currently-inactive slot (A or B,
/// whichever the user isn't editing), then flip `current_is_b`. Drives
/// the standard "copy A → B" pattern in DAW plugin UIs.
pub fn ab_copy_to_other(
    snap: &AbSnapshot,
    params: &[AtomicF32],
    dirty: &[AtomicBool],
) {
    let was_b = snap.current_is_b.load(Ordering::Relaxed);
    let target = if was_b { &snap.a } else { &snap.b };
    let mut t = target.lock().unwrap();
    if t.len() != params.len() {
        t.resize(params.len(), 0.0);
    }
    for (i, atom) in params.iter().enumerate() {
        t[i] = atom.load(Ordering::Relaxed);
    }
    // Don't actually swap which slot we're on — A→B means copy, user
    // stays on A. Use `ab_swap` for the actual swap.
    let _ = dirty;
}

/// Swap A↔B: snapshot live into current, then load the other slot back
/// into the live params. Raises dirty on every param so REAPER records
/// the switch into automation.
pub fn ab_swap(snap: &AbSnapshot, params: &[AtomicF32], dirty: &[AtomicBool]) {
    let was_b = snap.current_is_b.load(Ordering::Relaxed);
    let current = if was_b { &snap.b } else { &snap.a };
    let other = if was_b { &snap.a } else { &snap.b };
    // Save live into current slot.
    {
        let mut c = current.lock().unwrap();
        if c.len() != params.len() {
            c.resize(params.len(), 0.0);
        }
        for (i, atom) in params.iter().enumerate() {
            c[i] = atom.load(Ordering::Relaxed);
        }
    }
    // Load other slot into live + mark every param dirty.
    let o = other.lock().unwrap();
    for (i, atom) in params.iter().enumerate() {
        if let Some(v) = o.get(i).copied() {
            atom.store(v, Ordering::Relaxed);
            if let Some(d) = dirty.get(i) {
                d.store(true, Ordering::Relaxed);
            }
        }
    }
    snap.current_is_b.store(!was_b, Ordering::Relaxed);
}

/// Restore every param to its declared default. Marks every param dirty
/// so REAPER records the init.
pub fn init_params(params: &[AtomicF32], defs: &[ParamDef], dirty: &[AtomicBool]) {
    for (i, atom) in params.iter().enumerate() {
        if let Some(def) = defs.get(i) {
            atom.store(def.default as f32, Ordering::Relaxed);
        }
        if let Some(d) = dirty.get(i) {
            d.store(true, Ordering::Relaxed);
        }
    }
}

/// Render an A/B/Copy/Init row. Returns nothing — buttons act on the
/// shared state passed in.
pub fn ab_init_bar(
    ui: &mut egui::Ui,
    snap: &AbSnapshot,
    params: &[AtomicF32],
    defs: &[ParamDef],
    dirty: &[AtomicBool],
) {
    ui.horizontal(|ui| {
        let on_b = snap.current_is_b.load(Ordering::Relaxed);
        let a_label = if on_b { "A" } else { "[A]" };
        let b_label = if on_b { "[B]" } else { "B" };
        if ui.button(a_label).clicked() && on_b {
            ab_swap(snap, params, dirty);
        }
        if ui.button(b_label).clicked() && !on_b {
            ab_swap(snap, params, dirty);
        }
        if ui.button("copy →").on_hover_text("copy current to the other slot").clicked() {
            ab_copy_to_other(snap, params, dirty);
        }
        if ui.button("init").on_hover_text("reset every param to its default").clicked() {
            init_params(params, defs, dirty);
        }
    });
}

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

/// Bridge to the audio thread for emitting CLAP gesture begin/end events.
/// Holds two parallel flag arrays — one for "user just started dragging",
/// one for "user just released". Audio thread swaps both during `process()`
/// and pushes the appropriate events into the host's output queue.
///
/// The GUI doesn't care about the audio thread's read pattern — it only
/// needs to flip the bit on `drag_started` / `drag_stopped`. The audio
/// thread holds the only consumer view.
#[derive(Copy, Clone)]
pub struct GestureBridge<'a> {
    pub begin: &'a [AtomicBool],
    pub end: &'a [AtomicBool],
}

/// Same as `dirty_param_row` but also fires CLAP gesture begin/end events
/// when the user touches and releases the slider. Inlines the slider so we
/// can read `drag_started` / `drag_stopped` — those fire even on a no-op
/// click, which is exactly the semantic CLAP touch automation expects.
pub fn dirty_param_row_g(
    ui: &mut egui::Ui,
    atom: &AtomicF32,
    def: &ParamDef,
    dirty: &AtomicBool,
    gesture: GestureBridge<'_>,
    param_idx: usize,
) {
    ui.horizontal(|ui| {
        let name = std::str::from_utf8(def.name)
            .unwrap_or("?")
            .trim_end_matches('\0');
        ui.add_sized(
            [120.0, 18.0],
            egui::Label::new(
                egui::RichText::new(name).color(GREEN).monospace(),
            ),
        );
        let mut value = atom.load(Ordering::Relaxed);
        let slider = egui::Slider::new(&mut value, (def.min as f32)..=(def.max as f32))
            .show_value(true)
            .clamping(egui::SliderClamping::Always)
            .suffix(if def.unit.is_empty() {
                String::new()
            } else {
                format!(" {}", def.unit)
            });
        let resp = ui.add(slider);
        if resp.drag_started() {
            if let Some(b) = gesture.begin.get(param_idx) {
                b.store(true, Ordering::Relaxed);
            }
        }
        if resp.changed() {
            atom.store(value, Ordering::Relaxed);
            dirty.store(true, Ordering::Relaxed);
        }
        if resp.drag_stopped() {
            if let Some(e) = gesture.end.get(param_idx) {
                e.store(true, Ordering::Relaxed);
            }
        }
    });
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
