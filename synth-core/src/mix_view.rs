//! The mix map, for any plugin window: every track on the bus as a curve,
//! conflict heat per band, the most persistent clashes. Spectrum shows it
//! full size; every other plugin opens it from the `[ ] mix` toggle in its
//! top bar, focused on its own track — the EQ sees what it is fighting.
//!
//! State lives in egui memory per window (`MixView::shared`), so a plugin's
//! own GUI struct needs nothing new.

use crate::gui::{GREEN, GREEN_DIM, GREEN_FAINT, PANEL_BG};
use crate::mixmap::{self, Clash, MaskingTracker, TrackSpectrum, BANDS};
use egui::Color32;
use std::sync::Arc;
use superduper_dsp_sdk::bus::{band_edges, Reader};

const MIN_DB: f32 = -90.0;
const MAX_DB: f32 = 0.0;
const MIN_HZ: f32 = 20.0;
const MAX_HZ: f32 = 20_000.0;
/// Without a focus only this many of the loudest tracks are drawn: 40–100
/// tracks would otherwise be a tangle.
const MAX_CURVES: usize = 8;
const TRACK_COLORS: [Color32; 10] = [
    Color32::from_rgb(90, 200, 255), Color32::from_rgb(255, 150, 70), Color32::from_rgb(120, 230, 120),
    Color32::from_rgb(240, 90, 120), Color32::from_rgb(200, 140, 255), Color32::from_rgb(250, 220, 80),
    Color32::from_rgb(80, 220, 200), Color32::from_rgb(255, 120, 220), Color32::from_rgb(170, 190, 90),
    Color32::from_rgb(160, 170, 255),
];

/// Display knobs the host plugin may own (Spectrum maps its top row here).
#[derive(Clone, Copy)]
pub struct MixOpts {
    /// dB/oct around 1 kHz, as the Spectrum view.
    pub tilt: f32,
    /// One-pole per frame on each curve, 0..0.98.
    pub smoothing: f32,
    /// How long masking remembers, seconds.
    pub window_s: f32,
    /// Hold the picture.
    pub frozen: bool,
}

impl Default for MixOpts {
    fn default() -> Self { Self { tilt: 3.0, smoothing: 0.7, window_s: 8.0, frozen: false } }
}

/// dB a tilt of `db_per_oct` adds at `hz`, pivoting at 1 kHz.
pub fn tilt_db(hz: f32, db_per_oct: f32) -> f32 {
    (hz.max(20.0) / 1000.0).log2() * db_per_oct
}

pub struct MixView {
    bus: Option<Reader>,
    tracks: Vec<TrackSpectrum>,
    masking: MaskingTracker,
    window_s: f32,
    clashes: Vec<Clash>,
    polled: std::time::Instant,
    focus: Option<String>,
    focus_pair: Option<(String, String)>,
    /// The track this plugin sits on: the default focus, once.
    own_applied: bool,
    show_buses: bool,
}

impl Default for MixView {
    fn default() -> Self { Self::new() }
}

impl MixView {
    pub fn new() -> Self {
        Self {
            bus: None,
            tracks: Vec::new(),
            masking: MaskingTracker::new(80.0),
            window_s: 0.0,
            clashes: Vec::new(),
            polled: std::time::Instant::now(),
            focus: None,
            focus_pair: None,
            own_applied: false,
            show_buses: true,
        }
    }

    /// This window's view, kept in egui memory under `id`.
    pub fn shared(ctx: &egui::Context, id: egui::Id) -> Arc<parking_lot::Mutex<MixView>> {
        ctx.data_mut(|d| d.get_temp_mut_or_insert_with(id, || Arc::new(parking_lot::Mutex::new(MixView::new()))).clone())
    }

    fn poll(&mut self, opts: &MixOpts) {
        if (opts.window_s - self.window_s).abs() > 0.05 {
            self.masking = MaskingTracker::new(opts.window_s.max(1.0) * 10.0);
            self.window_s = opts.window_s;
        }
        if self.bus.is_none() { self.bus = Reader::open(); }
        let (false, Some(r)) = (opts.frozen, &self.bus) else { return };
        // Every frame for the curves; masking counts 10 frames a second so
        // its shares do not depend on the repaint rate.
        let rows = r.read_all();
        let fresh = mixmap::loudest_per_track(rows.iter().map(|s| (s.track.as_str(), s.is_bus(), s.rms_db, &s.bands_db)));
        if self.polled.elapsed().as_millis() >= 100 {
            self.polled = std::time::Instant::now();
            self.masking.update(&fresh);
            self.clashes = self.masking.top(64);
        }
        let k = opts.smoothing.clamp(0.0, 0.98);
        let old = std::mem::take(&mut self.tracks);
        self.tracks = fresh.into_iter().map(|mut t| {
            if let Some(o) = old.iter().find(|o| o.name == t.name) {
                for (b, ob) in t.bands_db.iter_mut().zip(o.bands_db.iter()) { *b = *ob * k + *b * (1.0 - k); }
            }
            t
        }).collect();
    }

    /// Draw into the rest of `ui`. `own_track`: the host plugin's track, the
    /// default focus (None in a plugin that is not on a track, like Spectrum
    /// on the master).
    pub fn ui(&mut self, ui: &mut egui::Ui, own_track: Option<&str>, opts: MixOpts) {
        self.poll(&opts);
        if !self.own_applied {
            if let Some(t) = own_track.filter(|t| self.tracks.iter().any(|x| x.name == *t && !x.bus)) {
                self.focus = Some(t.to_string());
                self.own_applied = true;
            }
        }
        let edges = band_edges();
        let centers: [f32; BANDS] = std::array::from_fn(|b| (edges[b] * edges[b + 1]).sqrt());
        let audible = |t: &TrackSpectrum| t.rms_db > mixmap::LOUD_DB;
        let mut names: Vec<String> = self.tracks.iter().filter(|t| !t.bus).map(|t| t.name.clone()).collect();
        names.sort();
        let color_of = |t: &TrackSpectrum| if t.bus {
            Color32::from_gray(150)
        } else {
            TRACK_COLORS[names.binary_search(&t.name).unwrap_or(0) % TRACK_COLORS.len()]
        };
        let label_of = |t: &TrackSpectrum| match (t.bus, t.name.is_empty()) {
            (true, true) => "unnamed (bus)".to_string(),
            (true, false) => format!("{} (bus)", t.name),
            _ => t.name.clone(),
        };

        // ── focus dropdown: scrolls, loudest first ──
        let mut by_level: Vec<&TrackSpectrum> = self.tracks.iter().filter(|t| self.show_buses || !t.bus).collect();
        by_level.sort_by(|a, b| b.rms_db.total_cmp(&a.rms_db));
        let playing = by_level.iter().filter(|t| audible(t) && !t.bus).count();
        let mut new_focus: Option<Option<String>> = None;
        let mut show_buses = self.show_buses;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("focus:").color(GREEN).monospace());
            egui::ComboBox::from_id_salt("mix_focus")
                .selected_text(self.focus.as_deref().unwrap_or("all tracks"))
                .width(240.0)
                .height(420.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(self.focus.is_none(), "all tracks").clicked() { new_focus = Some(None); }
                    for t in &by_level {
                        let col = if audible(t) { color_of(t) } else { color_of(t).gamma_multiply(0.4) };
                        let mine = own_track == Some(t.name.as_str());
                        let text = egui::RichText::new(format!("● {}{}  {:.0} dB", label_of(t), if mine { " (this)" } else { "" }, t.rms_db)).color(col);
                        if ui.selectable_label(self.focus.as_deref() == Some(t.name.as_str()), text).clicked() {
                            new_focus = Some(Some(t.name.clone()));
                        }
                    }
                });
            ui.checkbox(&mut show_buses, egui::RichText::new("master").color(GREEN_DIM).small());
            ui.label(egui::RichText::new(format!("{} tracks, {playing} playing", names.len())).color(GREEN_DIM).small());
        });
        self.show_buses = show_buses;
        if let Some(f) = new_focus {
            self.focus = f;
            self.focus_pair = None;
        }

        // ── clash list ──
        let clashes: Vec<&Clash> = self.clashes.iter()
            .filter(|c| self.focus.as_deref().is_none_or(|f| c.a == f || c.b == f))
            .take(4).collect();
        ui.label(egui::RichText::new(format!("masking — both loud and within 6 dB, share of the last ~{:.0} s; click one to see it:", opts.window_s)).color(GREEN_DIM).small());
        if clashes.is_empty() {
            ui.label(egui::RichText::new("no clashes (or nothing playing)").color(GREEN_DIM).monospace());
        }
        let mut new_pair: Option<Option<(String, String)>> = None;
        for c in &clashes {
            let f = |hz: f32| if hz >= 1000.0 { format!("{:.1}k", hz / 1000.0) } else { format!("{hz:.0}") };
            let key = (c.a.clone(), c.b.clone());
            let on = self.focus_pair.as_ref() == Some(&key);
            let text = egui::RichText::new(format!("{} × {}   {}–{} Hz   {:.0}%", c.a, c.b, f(edges[c.band]), f(edges[c.band + 1]), c.share * 100.0))
                .color(if c.share > 0.5 { Color32::from_rgb(255, 120, 110) } else { GREEN })
                .monospace();
            if ui.selectable_label(on, text).clicked() { new_pair = Some((!on).then_some(key)); }
        }
        if let Some(p) = new_pair { self.focus_pair = p; }

        // ── which curves: the pair, or the focus and its opponents, or the loudest few ──
        let lit = |t: &TrackSpectrum| match (&self.focus_pair, &self.focus) {
            (Some((a, b)), _) => t.name == *a || t.name == *b,
            (None, Some(f)) => t.name == *f,
            _ => false,
        };
        let shown: Vec<&TrackSpectrum> = match (&self.focus_pair, &self.focus) {
            (Some(_), _) => by_level.iter().copied().filter(|t| t.bus || lit(t)).collect(),
            (None, Some(f)) => by_level.iter().copied().filter(|t| {
                t.bus || t.name == *f || clashes.iter().any(|c| (c.a == t.name || c.b == t.name) && (c.a == *f || c.b == *f))
            }).collect(),
            _ => {
                let loud: Vec<&TrackSpectrum> = by_level.iter().copied().filter(|t| !t.bus && audible(t)).take(MAX_CURVES).collect();
                by_level.iter().copied().filter(|t| t.bus || loud.iter().any(|l| std::ptr::eq(*l, *t))).collect()
            }
        };
        let hidden = playing.saturating_sub(shown.iter().filter(|t| !t.bus && audible(t)).count());
        if hidden > 0 && self.focus.is_none() {
            ui.label(egui::RichText::new(format!("drawing the {MAX_CURVES} loudest; {hidden} more playing — focus one to see it")).color(GREEN_DIM).small());
        }

        // ── the graph takes what is left ──
        let avail = ui.available_rect_before_wrap();
        let rect = egui::Rect::from_min_size(avail.min, egui::vec2(avail.width(), avail.height().max(140.0)));
        ui.allocate_rect(rect, egui::Sense::hover());
        let painter = ui.painter_at(rect);
        let plot = egui::Rect::from_min_max(egui::pos2(rect.left() + 34.0, rect.top() + 6.0), egui::pos2(rect.right() - 6.0, rect.bottom() - 16.0));
        let (log_min, log_max) = (MIN_HZ.ln(), MAX_HZ.ln());
        let fx = |hz: f32| plot.left() + (hz.clamp(MIN_HZ, MAX_HZ).ln() - log_min) / (log_max - log_min) * plot.width();
        let fy = |db: f32| plot.top() + (MAX_DB - db.clamp(MIN_DB, MAX_DB)) / (MAX_DB - MIN_DB) * plot.height();
        painter.rect_filled(rect, 0.0, PANEL_BG);
        for &hz in &[50.0_f32, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0] {
            let x = fx(hz);
            painter.line_segment([egui::pos2(x, plot.top()), egui::pos2(x, plot.bottom())], egui::Stroke::new(1.0, GREEN_FAINT));
            let s = if hz >= 1000.0 { format!("{:.0}k", hz / 1000.0) } else { format!("{hz:.0}") };
            painter.text(egui::pos2(x, rect.bottom() - 2.0), egui::Align2::CENTER_BOTTOM, s, egui::FontId::monospace(10.0), GREEN_DIM);
        }
        for db in (-84..=0).step_by(12) {
            let y = fy(db as f32);
            painter.line_segment([egui::pos2(plot.left(), y), egui::pos2(plot.right(), y)], egui::Stroke::new(1.0, GREEN_FAINT));
            painter.text(egui::pos2(plot.left() - 4.0, y), egui::Align2::RIGHT_CENTER, format!("{db}"), egui::FontId::monospace(9.0), GREEN_DIM);
        }
        let y_of = |t: &TrackSpectrum, b: usize| fy(t.bands_db[b] + tilt_db(centers[b], opts.tilt));

        // Conflict heat: bands tinted by how long they are fought over, and a
        // solid strip along the bottom.
        let heat = match &self.focus_pair {
            Some((a, b)) => self.masking.pair(a, b).unwrap_or([0.0; BANDS]),
            None => self.masking.band_heat(self.focus.as_deref()),
        };
        for (b, h) in heat.iter().enumerate() {
            if *h < 0.05 { continue; }
            let xs = fx(edges[b])..=fx(edges[b + 1]);
            let a = (h.min(1.0) * 255.0) as u8;
            painter.rect_filled(egui::Rect::from_x_y_ranges(xs.clone(), plot.top()..=plot.bottom()), 0.0, Color32::from_rgba_unmultiplied(255, 60, 60, a / 5));
            painter.rect_filled(egui::Rect::from_x_y_ranges(xs, (plot.bottom() - 10.0)..=plot.bottom()), 0.0, Color32::from_rgba_unmultiplied(255, 60, 60, a));
        }
        // Pair view: the gap between the two curves, filled where they fight.
        if let Some((a, b)) = &self.focus_pair {
            let find = |n: &str| self.tracks.iter().find(|t| t.name == n);
            if let (Some(ta), Some(tb)) = (find(a), find(b)) {
                for (i, h) in heat.iter().enumerate() {
                    if *h < 0.1 { continue; }
                    let (ya, yb) = (y_of(ta, i), y_of(tb, i));
                    let r = egui::Rect::from_x_y_ranges(fx(edges[i])..=fx(edges[i + 1]), (ya.min(yb) - 3.0)..=(ya.max(yb) + 3.0));
                    painter.rect_filled(r, 2.0, Color32::from_rgba_unmultiplied(255, 70, 70, (h.min(1.0) * 200.0) as u8));
                }
            }
        }
        let any_focus = self.focus.is_some() || self.focus_pair.is_some();
        let mut order = shown;
        order.sort_by_key(|t| lit(t)); // lit last = drawn on top
        for t in order {
            let pts: Vec<egui::Pos2> = (0..BANDS).map(|b| egui::pos2(fx(centers[b]), y_of(t, b))).collect();
            let on = lit(t);
            let c = color_of(t);
            let width = if t.bus { 1.0 } else if on { 2.6 } else { 1.6 };
            painter.add(egui::Shape::line(pts, egui::Stroke::new(width, if any_focus && !on { c.gamma_multiply(0.3) } else { c })));
        }
    }
}

/// The `[ ] mix` toggle plus its floating window, for a plugin's top bar.
/// `scope` gives the plugin's own track (the window's default focus).
pub fn mix_toggle(ui: &mut egui::Ui, scope: &crate::gui::LiveScope) {
    let ctx = ui.ctx().clone();
    let open_id = egui::Id::new("sdsp_mix_open");
    let mut open = ctx.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false));
    let label = if open { "[X] mix" } else { "[ ] mix" };
    if ui.selectable_label(open, egui::RichText::new(label).color(GREEN).monospace())
        .on_hover_text("the whole mix from the SuperDuper bus: who masks this track, where")
        .clicked()
    {
        open = !open;
    }
    if open {
        let own = scope.track.lock().0.clone();
        let view = MixView::shared(&ctx, egui::Id::new("sdsp_mix_view"));
        let screen = ctx.content_rect();
        egui::Window::new(egui::RichText::new("mix").color(GREEN).monospace())
            .id(egui::Id::new("sdsp_mix_window"))
            .open(&mut open)
            .default_size(egui::vec2((screen.width() - 40.0).min(720.0), (screen.height() - 80.0).min(460.0)))
            .default_pos(screen.min + egui::vec2(20.0, 60.0))
            .resizable(true)
            .collapsible(false)
            .frame(egui::Frame::window(&ctx.style()).fill(PANEL_BG))
            .show(&ctx, |ui| view.lock().ui(ui, (!own.is_empty()).then_some(own.as_str()), MixOpts::default()));
    }
    ctx.data_mut(|d| d.insert_temp(open_id, open));
}
