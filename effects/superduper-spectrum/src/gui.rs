//! egui_baseview GUI for SuperDuper Spectrum.
//!
//! Three view modes (selected via the Mode CLAP param):
//!   - Spectrum (default): real-time line/bars over log-frequency.
//!   - Spectrogram: waterfall heatmap (x = time, y = log frequency,
//!     colour = dB). Like iZotope Insight's sonograph.
//!   - Split: spectrum on top, spectrogram below.
//!
//! Implementation notes:
//!   - The waterfall keeps a fixed-width column history. Every GUI frame
//!     we shift one column left and write the latest FFT magnitudes into
//!     the right edge. Column count is derived from `Window` (seconds)
//!     and a 30 Hz refresh estimate — close enough for visual scroll.
//!   - The colour image is uploaded as an egui texture each frame. For
//!     the typical sizes (200–600 columns × 128 bins) this is cheap.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use rtrb::Consumer;
use superduper_synth_core::analysis::magnitude_spectrum_db;
use superduper_synth_core::gui as core_gui;

use crate::palette::{db_to_color, Palette};
use crate::ring::SlidingHistory;
use crate::{P_FFT_SIZE, P_MODE, P_PALETTE, P_SMOOTHING, P_TILT, P_WINDOW, PARAMS, SharedParams};

pub const DEFAULT_WIDTH: u32 = 760;
pub const DEFAULT_HEIGHT: u32 = 520;
pub const MIN_WIDTH: u32 = 480;
pub const MIN_HEIGHT: u32 = 360;
pub const MAX_WIDTH: u32 = 1800;
pub const MAX_HEIGHT: u32 = 1400;

const MIN_DB: f32 = -90.0;
const MAX_DB: f32 = 0.0;
const MIN_HZ: f32 = 20.0;
const MAX_HZ: f32 = 20_000.0;
const SAMPLE_RATE_GUESS: f32 = 48_000.0;
/// Frequency bins used by the spectrogram (resampled from the FFT bins so
/// the waterfall texture doesn't depend on FFT size).
const SPEC_BINS: usize = 128;
/// Refresh estimate used to size the column history.
const REFRESH_HZ: f32 = 30.0;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

#[derive(Copy, Clone, Debug)]
enum Mode {
    Spectrum,
    Spectrogram,
    Split,
}

impl Mode {
    fn from_param(v: f32) -> Self {
        match v.round() as i32 {
            1 => Self::Spectrogram,
            2 => Self::Split,
            _ => Self::Spectrum,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Spectrum => "Spectrum",
            Self::Spectrogram => "Spectrogram",
            Self::Split => "Split",
        }
    }
}

struct GuiState {
    shared: SharedParams,
    resize: ResizeBridge,
    applied_size: (u32, u32),

    consumer: Consumer<f32>,
    history: SlidingHistory,
    linear_scratch: Vec<f32>,
    smoothed: Vec<f32>,
    current_fft_size: usize,

    /// Column-major history for the spectrogram: each entry = one frame's
    /// `SPEC_BINS`-wide magnitude row (newest at the back).
    waterfall: VecDeque<Vec<f32>>,
    waterfall_capacity: usize,
    waterfall_tex: Option<TextureHandle>,

    /// Mix view: every SuperDuper plugin's track spectrum from the bus.
    mix: bool,
    bus: Option<superduper_dsp_sdk::bus::Reader>,
    bus_tracks: Vec<superduper_synth_core::mixmap::TrackSpectrum>,
    masking: superduper_synth_core::mixmap::MaskingTracker,
    bus_polled: std::time::Instant,
    focus: Option<String>,
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P,
    shared: SharedParams,
    consumer: Option<Consumer<f32>>,
    resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Spectrum".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let consumer = consumer.unwrap_or_else(|| {
        let (_p, c) = rtrb::RingBuffer::<f32>::new(64);
        c
    });

    let fft_size = clamp_fft(shared.params[P_FFT_SIZE].load(Ordering::Relaxed) as usize);
    let waterfall_capacity = (shared.params[P_WINDOW].load(Ordering::Relaxed) * REFRESH_HZ) as usize;
    let waterfall_capacity = waterfall_capacity.clamp(32, 1200);

    let state = GuiState {
        shared,
        resize,
        applied_size: (initial_w, initial_h),
        consumer,
        history: SlidingHistory::new(fft_size),
        linear_scratch: Vec::with_capacity(fft_size),
        smoothed: vec![MIN_DB; fft_size / 2 + 1],
        current_fft_size: fft_size,
        waterfall: VecDeque::with_capacity(waterfall_capacity),
        waterfall_capacity,
        waterfall_tex: None,
        mix: false,
        bus: None,
        bus_tracks: Vec::new(),
        // 10 updates a second, an ~8 s memory: a clash on every chorus hit
        // stays lit, one stray transient does not.
        masking: superduper_synth_core::mixmap::MaskingTracker::new(80.0),
        bus_polled: std::time::Instant::now(),
        focus: None,
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

fn clamp_fft(raw: usize) -> usize {
    *[1024_usize, 2048, 4096, 8192]
        .iter()
        .min_by_key(|c| ((**c as i64) - (raw as i64)).abs())
        .unwrap()
}

fn draw(ctx: &egui::Context, state: &mut GuiState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        // ── Top bar ──
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("sdsp> SPECTRUM")
                    .color(core_gui::GREEN_BRIGHT)
                    .monospace()
                    .strong(),
            );
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!(
                    "b{} {}",
                    env!("SDSP_BUILD_NUM"),
                    env!("SDSP_BUILD_DATE")
                ))
                .color(core_gui::GREEN_DIM)
                .monospace(),
            );
        });

        // ── BS.1770 loudness + true-peak readout ──
        // Mastering-grade meter — Momentary (400 ms) / Short-term (3 s)
        // / Integrated (full program, gated) all K-weighted, plus
        // true-peak in dBTP (inter-sample-peak via 4× upsample).
        core_gui::loudness_row(
            ui,
            Some(state.shared.lufs_momentary.load(Ordering::Relaxed)),
            state.shared.lufs_short_term.load(Ordering::Relaxed),
            state.shared.lufs_integrated.load(Ordering::Relaxed),
            state.shared.true_peak_dbtp.load(Ordering::Relaxed),
            None,
        );

        // ── Mode + Palette + Freeze + FFT/Smoothing/Tilt/Window ──
        ui.horizontal(|ui| {
            let raw_mode = state.shared.params[P_MODE].load(Ordering::Relaxed);
            let mut mode = Mode::from_param(raw_mode);
            ui.label(egui::RichText::new("mode:").color(core_gui::GREEN).monospace());
            egui::ComboBox::from_id_salt("spec_mode")
                .selected_text(mode.name())
                .width(120.0)
                .show_ui(ui, |ui| {
                    for (i, m) in [Mode::Spectrum, Mode::Spectrogram, Mode::Split].iter().enumerate() {
                        if ui
                            .selectable_label(matches!(mode, _x if std::mem::discriminant(&mode) == std::mem::discriminant(m)), m.name())
                            .clicked()
                        {
                            mode = *m;
                            state.shared.params[P_MODE].store(i as f32, Ordering::Relaxed);
                        }
                    }
                });

            ui.add_space(10.0);
            ui.label(egui::RichText::new("palette:").color(core_gui::GREEN).monospace());
            let palette_idx = state.shared.params[P_PALETTE].load(Ordering::Relaxed) as u32;
            let palette = Palette::from_index(palette_idx);
            egui::ComboBox::from_id_salt("spec_palette")
                .selected_text(palette.name())
                .width(110.0)
                .show_ui(ui, |ui| {
                    for (i, p) in [Palette::Phosphor, Palette::Heat, Palette::Mono].iter().enumerate() {
                        if ui
                            .selectable_label(palette_idx as usize == i, p.name())
                            .clicked()
                        {
                            state.shared.params[P_PALETTE].store(i as f32, Ordering::Relaxed);
                        }
                    }
                });

            ui.add_space(10.0);
            let mut frozen = state.shared.bypass.load(Ordering::Relaxed);
            let label = if frozen { "[X] freeze" } else { "[ ] freeze" };
            if ui
                .selectable_label(frozen, egui::RichText::new(label).color(core_gui::GREEN).monospace())
                .clicked()
            {
                frozen = !frozen;
                state.shared.bypass.store(frozen, Ordering::Relaxed);
            }
            ui.add_space(10.0);
            let label = if state.mix { "[X] mix" } else { "[ ] mix" };
            if ui.selectable_label(state.mix, egui::RichText::new(label).color(core_gui::GREEN).monospace())
                .on_hover_text("every SuperDuper plugin's track on one graph, and who masks whom")
                .clicked()
            {
                state.mix = !state.mix;
            }
        });
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("fft:").color(core_gui::GREEN).monospace());
            let raw = state.shared.params[P_FFT_SIZE].load(Ordering::Relaxed) as usize;
            let fft_size = clamp_fft(raw);
            egui::ComboBox::from_id_salt("fft_size_combo")
                .selected_text(format!("{}", fft_size))
                .width(80.0)
                .show_ui(ui, |ui| {
                    for opt in [1024_usize, 2048, 4096, 8192] {
                        if ui.selectable_label(fft_size == opt, format!("{}", opt)).clicked() {
                            state.shared.params[P_FFT_SIZE].store(opt as f32, Ordering::Relaxed);
                        }
                    }
                });
            ui.add_space(6.0);
            core_gui::param_row(ui, &state.shared.params[P_SMOOTHING], &PARAMS[P_SMOOTHING]);
        });
        ui.horizontal(|ui| {
            core_gui::param_row(ui, &state.shared.params[P_TILT], &PARAMS[P_TILT]);
            core_gui::param_row(ui, &state.shared.params[P_WINDOW], &PARAMS[P_WINDOW]);
        });
        ui.add_space(4.0);

        // ── Recompute analysis state ──
        update_analysis(state);

        // ── Render ──
        let mode = Mode::from_param(state.shared.params[P_MODE].load(Ordering::Relaxed));
        let palette = Palette::from_index(state.shared.params[P_PALETTE].load(Ordering::Relaxed) as u32);
        let tilt = state.shared.params[P_TILT].load(Ordering::Relaxed);
        if state.mix {
            draw_mix_panel(ui, state);
            return;
        }
        match mode {
            Mode::Spectrum => {
                let rect = allocate_plot(ui);
                draw_spectrum_panel(ui, rect, &state.smoothed, state.current_fft_size, tilt);
            }
            Mode::Spectrogram => {
                let rect = allocate_plot(ui);
                draw_spectrogram_panel(ui, rect, state, palette);
            }
            Mode::Split => {
                // Top half: spectrum line
                let avail = ui.available_rect_before_wrap();
                let half_h = (avail.height() / 2.0 - 4.0).max(80.0);
                let top_rect = egui::Rect::from_min_size(avail.min, egui::vec2(avail.width(), half_h));
                let bot_rect = egui::Rect::from_min_size(
                    egui::pos2(avail.min.x, avail.min.y + half_h + 8.0),
                    egui::vec2(avail.width(), half_h),
                );
                ui.allocate_rect(avail, egui::Sense::hover());
                draw_spectrum_panel(ui, top_rect, &state.smoothed, state.current_fft_size, tilt);
                draw_spectrogram_panel(ui, bot_rect, state, palette);
            }
        }
    });
}

fn allocate_plot(ui: &mut egui::Ui) -> egui::Rect {
    let size = egui::vec2(ui.available_width(), ui.available_height().max(160.0));
    let (rect, _resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    rect
}

fn update_analysis(state: &mut GuiState) {
    // Apply FFT size change if user picked a different value.
    let fft_size_now =
        clamp_fft(state.shared.params[P_FFT_SIZE].load(Ordering::Relaxed) as usize);
    if fft_size_now != state.current_fft_size {
        state.history.resize(fft_size_now);
        state.smoothed = vec![MIN_DB; fft_size_now / 2 + 1];
        state.current_fft_size = fft_size_now;
    }
    // Re-size waterfall capacity when Window seconds changes.
    let target_cap = (state.shared.params[P_WINDOW].load(Ordering::Relaxed) * REFRESH_HZ) as usize;
    let target_cap = target_cap.clamp(32, 1200);
    if target_cap != state.waterfall_capacity {
        while state.waterfall.len() > target_cap {
            state.waterfall.pop_front();
        }
        state.waterfall_capacity = target_cap;
    }

    let frozen = state.shared.bypass.load(Ordering::Relaxed);
    if frozen {
        return;
    }

    state.history.drain_from(&mut state.consumer);
    state.history.linear(&mut state.linear_scratch);
    let spec = magnitude_spectrum_db(&state.linear_scratch);

    let smoothing = state.shared.params[P_SMOOTHING]
        .load(Ordering::Relaxed)
        .clamp(0.0, 1.0);
    let alpha = 1.0 - smoothing;
    for (s, new) in state.smoothed.iter_mut().zip(spec.iter()) {
        *s = *s * (1.0 - alpha) + *new * alpha;
    }

    // Resample current spectrum into SPEC_BINS log-spaced cells for the
    // spectrogram texture.
    let mut row = vec![MIN_DB; SPEC_BINS];
    resample_log(&state.smoothed, state.current_fft_size, &mut row);
    if state.waterfall.len() >= state.waterfall_capacity {
        state.waterfall.pop_front();
    }
    state.waterfall.push_back(row);
}

/// Take FFT bins (linear in frequency) and resample into `out_bins`
/// log-spaced cells between MIN_HZ and MAX_HZ. Each output cell is the max
/// of the FFT bins that fall inside its frequency window.
fn resample_log(spectrum: &[f32], fft_size: usize, out: &mut [f32]) {
    let sr = SAMPLE_RATE_GUESS;
    let log_min = MIN_HZ.ln();
    let log_max = MAX_HZ.ln();
    let n = out.len();

    for (i, cell) in out.iter_mut().enumerate() {
        let f_lo = (log_min + (log_max - log_min) * (i as f32) / n as f32).exp();
        let f_hi = (log_min + (log_max - log_min) * (i + 1) as f32 / n as f32).exp();
        let bin_lo = ((f_lo * fft_size as f32) / sr).floor() as usize;
        let bin_hi = ((f_hi * fft_size as f32) / sr).ceil() as usize;
        let bin_hi = bin_hi.min(spectrum.len());
        let bin_lo = bin_lo.min(bin_hi);
        let mut peak = MIN_DB;
        for s in &spectrum[bin_lo..bin_hi] {
            if *s > peak {
                peak = *s;
            }
        }
        *cell = peak;
    }
}

fn draw_axes(painter: &egui::Painter, rect: egui::Rect, plot: egui::Rect) {
    let grid = core_gui::GREEN_FAINT;
    let label = core_gui::GREEN_DIM;
    let log_min = MIN_HZ.ln();
    let log_max = MAX_HZ.ln();
    let freq_to_x = |hz: f32| -> f32 {
        let frac = (hz.clamp(MIN_HZ, MAX_HZ).ln() - log_min) / (log_max - log_min);
        plot.left() + frac * plot.width()
    };
    let db_to_y = |db: f32| -> f32 {
        let frac = (MAX_DB - db.clamp(MIN_DB, MAX_DB)) / (MAX_DB - MIN_DB);
        plot.top() + frac * plot.height()
    };

    painter.rect_filled(rect, 0.0, core_gui::PANEL_BG);
    painter.rect_stroke(
        rect,
        0.0,
        egui::Stroke::new(1.0, core_gui::GREEN_FAINT),
        egui::StrokeKind::Inside,
    );

    for &hz in &[50.0_f32, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0] {
        let x = freq_to_x(hz);
        painter.line_segment(
            [egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())],
            egui::Stroke::new(1.0, grid),
        );
        let s = if hz >= 1000.0 { format!("{:.0}k", hz / 1000.0) } else { format!("{:.0}", hz) };
        painter.text(
            egui::pos2(x, rect.bottom() - 4.0),
            egui::Align2::CENTER_BOTTOM,
            s,
            egui::FontId::monospace(10.0),
            label,
        );
    }
    for db in (-84..=0).step_by(12) {
        let y = db_to_y(db as f32);
        painter.line_segment(
            [egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)],
            egui::Stroke::new(1.0, grid),
        );
        painter.text(
            egui::pos2(rect.left() + 36.0, y),
            egui::Align2::RIGHT_CENTER,
            format!("{}", db),
            egui::FontId::monospace(9.0),
            label,
        );
    }
}

fn plot_inset(rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(rect.left() + 40.0, rect.top() + 8.0),
        egui::pos2(rect.right() - 6.0, rect.bottom() - 18.0),
    )
}

fn draw_spectrum_panel(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    smoothed: &[f32],
    fft_size: usize,
    tilt_db_per_oct: f32,
) {
    let painter = ui.painter_at(rect);
    let plot = plot_inset(rect);
    draw_axes(&painter, rect, plot);

    let sr = SAMPLE_RATE_GUESS;
    let n = fft_size as f32;
    let log_min = MIN_HZ.ln();
    let log_max = MAX_HZ.ln();
    let freq_to_x = |hz: f32| -> f32 {
        let frac = (hz.clamp(MIN_HZ, MAX_HZ).ln() - log_min) / (log_max - log_min);
        plot.left() + frac * plot.width()
    };
    let db_to_y = |db: f32| -> f32 {
        let frac = (MAX_DB - db.clamp(MIN_DB, MAX_DB)) / (MAX_DB - MIN_DB);
        plot.top() + frac * plot.height()
    };
    let tilt_at = |hz: f32| -> f32 {
        (hz.max(20.0) / 1000.0).log2() * tilt_db_per_oct
    };

    let bar_color = core_gui::GREEN_DIM.linear_multiply(0.55);
    let line_color = core_gui::GREEN_BRIGHT;
    let baseline_y = db_to_y(MIN_DB);
    let mut line_points: Vec<egui::Pos2> = Vec::with_capacity(smoothed.len());

    for (i, db) in smoothed.iter().enumerate() {
        let hz = i as f32 * sr / n;
        if hz < MIN_HZ || hz > MAX_HZ { continue; }
        let display_db = *db + tilt_at(hz);
        let x = freq_to_x(hz);
        let y = db_to_y(display_db).max(plot.top()).min(baseline_y);
        painter.line_segment(
            [egui::pos2(x, baseline_y), egui::pos2(x, y)],
            egui::Stroke::new(1.0, bar_color),
        );
        line_points.push(egui::pos2(x, y));
    }
    if line_points.len() >= 2 {
        painter.add(egui::Shape::line(
            line_points,
            egui::Stroke::new(1.5, line_color),
        ));
    }
}

fn draw_spectrogram_panel(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    state: &mut GuiState,
    palette: Palette,
) {
    let painter = ui.painter_at(rect);
    let plot = plot_inset(rect);
    draw_axes(&painter, rect, plot);

    if state.waterfall.is_empty() {
        painter.text(
            plot.center(),
            egui::Align2::CENTER_CENTER,
            "(no data yet)",
            egui::FontId::monospace(11.0),
            core_gui::GREEN_DIM,
        );
        return;
    }

    // Build a ColorImage column-by-column. Height = SPEC_BINS, width =
    // number of waterfall frames. Frequency axis is log-spaced both in
    // resample_log() and in the y mapping below — so a flat pink-noise
    // input looks like a flat horizontal band.
    let cols = state.waterfall.len();
    let rows = SPEC_BINS;
    let mut pixels = vec![Color32::BLACK; cols * rows];

    for (col, row_data) in state.waterfall.iter().enumerate() {
        for (bin, db) in row_data.iter().enumerate() {
            // egui ColorImage is row-major. Higher frequencies should appear
            // at the TOP of the image — invert.
            let y_row = (rows - 1) - bin;
            pixels[y_row * cols + col] = db_to_color(*db, MIN_DB, palette);
        }
    }
    let img = ColorImage {
        size: [cols, rows],
        pixels,
        source_size: egui::vec2(cols as f32, rows as f32),
    };

    let tex_id = match &mut state.waterfall_tex {
        Some(handle) => {
            handle.set(img, TextureOptions::LINEAR);
            handle.id()
        }
        None => {
            let handle = ui.ctx().load_texture("spectrogram", img, TextureOptions::LINEAR);
            let id = handle.id();
            state.waterfall_tex = Some(handle);
            id
        }
    };

    painter.image(
        tex_id,
        plot,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        Color32::WHITE,
    );
}

const TRACK_COLORS: [Color32; 10] = [
    Color32::from_rgb(90, 200, 255), Color32::from_rgb(255, 150, 70), Color32::from_rgb(120, 230, 120),
    Color32::from_rgb(240, 90, 120), Color32::from_rgb(200, 140, 255), Color32::from_rgb(250, 220, 80),
    Color32::from_rgb(80, 220, 200), Color32::from_rgb(255, 120, 220), Color32::from_rgb(170, 190, 90),
    Color32::from_rgb(160, 170, 255),
];

/// Mix view: one curve per track on the bus, the master dashed in grey, and
/// the most persistent clashes listed under the graph (the worst one shaded
/// on it). Click a track name to bring its curve forward.
fn draw_mix_panel(ui: &mut egui::Ui, state: &mut GuiState) {
    use superduper_synth_core::mixmap::{is_bus_name, loudest_per_track};
    if state.bus.is_none() { state.bus = superduper_dsp_sdk::bus::Reader::open(); }
    if state.bus_polled.elapsed().as_millis() >= 100 {
        state.bus_polled = std::time::Instant::now();
        if let Some(r) = &state.bus {
            let rows = r.read_all();
            state.bus_tracks = loudest_per_track(rows.iter().map(|s| (s.track.as_str(), s.rms_db, &s.bands_db)));
            state.masking.update(&state.bus_tracks);
        }
    }
    let edges = superduper_dsp_sdk::bus::band_edges();
    let clashes = state.masking.top(4);
    // Silent tracks (stopped, muted, not playing in this section) only crowd
    // the legend; count them instead.
    let audible = |t: &superduper_synth_core::mixmap::TrackSpectrum| t.rms_db > superduper_synth_core::mixmap::LOUD_DB;
    let names: Vec<String> = state.bus_tracks.iter().filter(|t| !is_bus_name(&t.name)).map(|t| t.name.clone()).collect();
    let color_of = |name: &str| TRACK_COLORS[names.iter().position(|n| n == name).unwrap_or(0) % TRACK_COLORS.len()];

    // Legend and clash list first, the graph takes what is left: a legend that
    // wraps to two lines can no longer push the list off the window.
    ui.horizontal_wrapped(|ui| {
        for t in state.bus_tracks.iter().filter(|t| audible(t)) {
            let label = if is_bus_name(&t.name) { format!("{} (bus)", if t.name.is_empty() { "unnamed" } else { &t.name }) } else { t.name.clone() };
            let col = if is_bus_name(&t.name) { Color32::from_gray(150) } else { color_of(&t.name) };
            let on = state.focus.as_deref() == Some(t.name.as_str());
            if ui.selectable_label(on, egui::RichText::new(format!("● {label}  {:.0} dB", t.rms_db)).color(col)).clicked() {
                state.focus = if on { None } else { Some(t.name.clone()) };
            }
        }
        let silent = state.bus_tracks.iter().filter(|t| !audible(t)).count();
        if silent > 0 {
            ui.label(egui::RichText::new(format!("+{silent} silent")).color(core_gui::GREEN_DIM).small());
        }
    });
    ui.label(egui::RichText::new("masking — both loud and within 6 dB, share of the last ~8 s:").color(core_gui::GREEN_DIM).small());
    if clashes.is_empty() {
        ui.label(egui::RichText::new("no clashes (or nothing playing)").color(core_gui::GREEN_DIM).monospace());
    }
    for c in &clashes {
        let lo = edges[c.band];
        let hi = edges[c.band + 1];
        let f = |hz: f32| if hz >= 1000.0 { format!("{:.1}k", hz / 1000.0) } else { format!("{hz:.0}") };
        ui.label(egui::RichText::new(format!("{} × {}   {}–{} Hz   {:.0}%", c.a, c.b, f(lo), f(hi), c.share * 100.0))
            .color(if c.share > 0.5 { Color32::from_rgb(255, 120, 110) } else { core_gui::GREEN })
            .monospace());
    }

    let avail = ui.available_rect_before_wrap();
    let rect = egui::Rect::from_min_size(avail.min, egui::vec2(avail.width(), avail.height().max(140.0)));
    ui.allocate_rect(rect, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let plot = plot_inset(rect);
    draw_axes(&painter, rect, plot);
    let log_min = MIN_HZ.ln();
    let log_max = MAX_HZ.ln();
    let fx = |hz: f32| plot.left() + (hz.clamp(MIN_HZ, MAX_HZ).ln() - log_min) / (log_max - log_min) * plot.width();
    let fy = |db: f32| plot.top() + (MAX_DB - db.clamp(MIN_DB, MAX_DB)) / (MAX_DB - MIN_DB) * plot.height();

    if let Some(c) = clashes.first() {
        let band = egui::Rect::from_x_y_ranges(fx(edges[c.band])..=fx(edges[c.band + 1]), plot.top()..=plot.bottom());
        painter.rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(255, 80, 80, 40));
    }
    let mut order: Vec<&superduper_synth_core::mixmap::TrackSpectrum> = state.bus_tracks.iter().filter(|t| audible(t)).collect();
    // Focused track last = drawn on top.
    order.sort_by_key(|t| state.focus.as_deref() == Some(t.name.as_str()));
    for t in order {
        let pts: Vec<egui::Pos2> = (0..edges.len() - 1)
            .map(|b| egui::pos2(fx((edges[b] * edges[b + 1]).sqrt()), fy(t.bands_db[b])))
            .collect();
        let focused = state.focus.as_deref() == Some(t.name.as_str());
        let dim = state.focus.is_some() && !focused;
        let stroke = if is_bus_name(&t.name) {
            egui::Stroke::new(1.0, Color32::from_gray(if dim { 60 } else { 130 }))
        } else {
            let c = color_of(&t.name);
            egui::Stroke::new(if focused { 2.6 } else { 1.6 }, if dim { c.gamma_multiply(0.3) } else { c })
        };
        painter.add(egui::Shape::line(pts, stroke));
    }
}
