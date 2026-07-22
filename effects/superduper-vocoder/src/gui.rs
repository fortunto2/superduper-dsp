//! egui_baseview GUI for SuperDuper Vocoder.
//!
//! Layout: top bar (name / build / bypass / preset) → A/B/init bar → live
//! spectrum strip → sectioned param rows (Modulator / Carrier / Character /
//! Output) → collapsible help.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use superduper_synth_core::gui as core_gui;

use crate::dsp::{MAX_BANDS, MODE_SPECTRAL};
use crate::presets::PRESETS;
use crate::viz::VIZ_CURVE;
use crate::{
    P_ATTACK, P_BANDS, P_DETAIL, P_DETUNE, P_DRIVE, P_FORMANT, P_MIX, P_MODE, P_OUTPUT, P_PITCH,
    P_PITCH_SOURCE, P_RELEASE, P_SOURCE, P_UNVOICED, P_WAVE, PARAMS, SharedParams,
};

pub const DEFAULT_WIDTH: u32 = 500;
pub const DEFAULT_HEIGHT: u32 = 560;
pub const MIN_WIDTH: u32 = 380;
pub const MIN_HEIGHT: u32 = 420;
pub const MAX_WIDTH: u32 = 1400;
pub const MAX_HEIGHT: u32 = 1200;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

const SOURCE_NAMES: [&str; 2] = ["Internal", "Sidechain"];
const WAVE_NAMES: [&str; 4] = ["Saw", "Square", "Pulse", "Saw+Sub"];
const BAND_NAMES: [&str; 3] = ["11 tinny", "16", "20 clear"];
const PITCH_SRC_NAMES: [&str; 3] = ["Auto", "MIDI", "Voice"];
const MODE_NAMES: [&str; 2] = ["Classic", "Spectral"];
const DETAIL_NAMES: [&str; 4] = ["Low", "Mid", "High", "Ultra"];

struct GuiState {
    shared: SharedParams,
    resize: ResizeBridge,
    applied_size: (u32, u32),
    selected_preset: Option<usize>,
    /// Slow-decaying peak for the activity display auto-gain (stable bars).
    viz_peak: f32,
    preset_names: Vec<&'static str>,
}

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P,
    shared: SharedParams,
    resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Vocoder".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let initial_preset_idx = (shared.active_preset.load(std::sync::atomic::Ordering::Relaxed)
        as usize)
        .min(PRESETS.len().saturating_sub(1));
    let state = GuiState {
        shared,
        resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(initial_preset_idx),
        viz_peak: 1e-4,
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

fn draw(ctx: &egui::Context, state: &mut GuiState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        if let Some(i) = core_gui::top_bar(
            ui,
            "SuperDuper Vocoder",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "vocoder_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            apply_preset(state, i);
        }

        core_gui::ab_init_bar(
            ui,
            &state.shared.ab_snapshot,
            &state.shared.params,
            PARAMS,
            &state.shared.dirty_params,
        );
        let (viz_rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 84.0), egui::Sense::hover());
        draw_vocoder_activity(ui, state, viz_rect);

        let gesture = || core_gui::GestureBridge {
            begin: &state.shared.gesture_begin,
            end: &state.shared.gesture_end,
        };

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Engine", |ui| {
                core_gui::dirty_choice_row_g(
                    ui,
                    &state.shared.params[P_MODE],
                    &PARAMS[P_MODE],
                    &MODE_NAMES,
                    &state.shared.dirty_params[P_MODE],
                    gesture(),
                    P_MODE,
                );
            });

            // In Spectral mode the band bank is replaced by an FFT, so the
            // 11/16/20 "bands" choice becomes a formant-envelope Detail control.
            let spectral =
                state.shared.params[P_MODE].load(std::sync::atomic::Ordering::Relaxed) >= 0.5;
            core_gui::section(ui, "Modulator", |ui| {
                param(ui, state, P_ATTACK);
                param(ui, state, P_RELEASE);
                if spectral {
                    core_gui::dirty_choice_row_g(
                        ui,
                        &state.shared.params[P_DETAIL],
                        &PARAMS[P_DETAIL],
                        &DETAIL_NAMES,
                        &state.shared.dirty_params[P_DETAIL],
                        gesture(),
                        P_DETAIL,
                    );
                } else {
                    core_gui::dirty_choice_row_g(
                        ui,
                        &state.shared.params[P_BANDS],
                        &PARAMS[P_BANDS],
                        &BAND_NAMES,
                        &state.shared.dirty_params[P_BANDS],
                        gesture(),
                        P_BANDS,
                    );
                }
            });

            core_gui::section(ui, "Carrier", |ui| {
                core_gui::dirty_choice_row_g(
                    ui,
                    &state.shared.params[P_SOURCE],
                    &PARAMS[P_SOURCE],
                    &SOURCE_NAMES,
                    &state.shared.dirty_params[P_SOURCE],
                    gesture(),
                    P_SOURCE,
                );
                core_gui::dirty_choice_row_g(
                    ui,
                    &state.shared.params[P_PITCH_SOURCE],
                    &PARAMS[P_PITCH_SOURCE],
                    &PITCH_SRC_NAMES,
                    &state.shared.dirty_params[P_PITCH_SOURCE],
                    gesture(),
                    P_PITCH_SOURCE,
                );
                core_gui::dirty_choice_row_g(
                    ui,
                    &state.shared.params[P_WAVE],
                    &PARAMS[P_WAVE],
                    &WAVE_NAMES,
                    &state.shared.dirty_params[P_WAVE],
                    gesture(),
                    P_WAVE,
                );
                param(ui, state, P_PITCH);
                param(ui, state, P_DETUNE);
            });

            core_gui::section(ui, "Character", |ui| {
                param(ui, state, P_FORMANT);
                param(ui, state, P_UNVOICED);
                param(ui, state, P_DRIVE);
            });

            core_gui::section(ui, "Output", |ui| {
                param(ui, state, P_MIX);
                param(ui, state, P_OUTPUT);
            });

            core_gui::help_block(
                ui,
                "vocoder_help",
                &[
                    (
                        "What is a vocoder?",
                        "It imprints the moving spectral shape of one sound (the MODULATOR — \
                         your voice on the main input) onto another (the CARRIER — a synth tone). \
                         Sixteen band-pass filters measure how loud your voice is in each slice \
                         of the spectrum, and the same 16 bands shape the carrier to match. The \
                         result talks with the carrier's timbre — a robot / choir / talkbox voice.",
                    ),
                    (
                        "Classic vs Spectral mode",
                        "Classic is the multi-band channel vocoder (11/16/20 band-pass filters — \
                         the hardware-vocoder sound, zero-latency). Spectral does the same idea \
                         with an FFT: it lifts your voice's whole magnitude envelope and imposes \
                         it on the carrier's spectrum — much finer formant detail and smoother, \
                         but it adds ~32 ms of latency (the DAW compensates via PDC). Formant \
                         Shift, Unvoiced and Drive work in both. Try Spectral for lush/clear \
                         robot pads, Classic for punchy talkbox.",
                    ),
                    (
                        "Internal vs Sidechain carrier",
                        "Internal: built-in oscillators (Saw / Square / Pulse / Saw+Sub) are \
                         pitch-tracked off your voice so you get a robot voice with no keyboard — \
                         Pitch offsets it in semitones, Detune fattens it. Sidechain: route any \
                         synth/pad to the 'Carrier' input (port 1) via the FX pin connector and \
                         it becomes the carrier — play chords on the synth for a musical vocoder.",
                    ),
                    (
                        "Bands / Detail / Formant / Unvoiced",
                        "In Classic, Band Count is the core character: 11 = old tinny robot \
                         (Talker / 'Harder Better Faster Stronger'), 20 = clearer modern R.A.M. \
                         In Spectral there are no bands — the same selector becomes Detail \
                         (Low…Ultra), the formant-envelope resolution (broad classic formants → \
                         fine full-FFT detail). \
                         Formant Shift moves the synthesis bands up/down in semitones without \
                         changing pitch — down = bigger/darker robot, up = chipmunk/tiny. \
                         Unvoiced feeds band-filtered NOISE into the top (sibilant) bands so \
                         consonants (s, t, f) come through naturally — turn it up if words \
                         sound mushy.",
                    ),
                    (
                        "Chain tips",
                        "Put a Compressor on the voice BEFORE this plugin — evening out the \
                         dynamics gives much tighter articulation (this is what the records do). \
                         For a fatter, chord-playable carrier than the internal osc, set Carrier \
                         Source = Sidechain and feed SuperDuper Wave (or any synth) into the \
                         'Carrier' input. Carrier Detune both fattens and widens the stereo \
                         image.",
                    ),
                    (
                        "Live rig (MIDI notes)",
                        "Route a MIDI keyboard to this FX track and play CHORDS to pitch the \
                         robot voice while you sing into the audio input — up to 6 notes at once \
                         (Herbie Hancock / Daft Punk vocoder chords). Pitch Src = Auto uses the \
                         keys when held and pitch-tracks your voice otherwise; MIDI = keys only \
                         (carrier silent with no keys held, like a hardware vocoder); Voice = \
                         ignore keys, always track the voice. Zero added latency — safe with the \
                         DAW's direct / low-latency monitoring.",
                    ),
                    (
                        "Tip: sing in tune",
                        "With the internal carrier, singing on-pitch to a scale or chord \
                         progression gives far more musical results — the tracked pitch drives \
                         the oscillators, so a steady note yields a steady robot tone. Add Drive \
                         for grit; keep Mix at 100% for a full robot, lower it to thicken a \
                         natural vocal.",
                    ),
                ],
            );
        });
    });
}

/// The vocoder-activity display — the hero visual. Classic draws one bouncing
/// bar per active band (band activity); Spectral draws the modulator's formant
/// envelope as a filled curve. Backed by the lock-free `VocViz` snapshot; a
/// slow-decaying peak in `GuiState` keeps the auto-gain stable. A fixed dark
/// strip background keeps the coloured bars legible in both light and dark.
fn draw_vocoder_activity(ui: &mut egui::Ui, state: &mut GuiState, rect: egui::Rect) {
    let painter = ui.painter_at(rect);
    let bg = egui::Color32::from_gray(18);
    painter.rect_filled(rect, 4.0, bg);

    let spectral = state.shared.viz.mode() == MODE_SPECTRAL;

    // Gather the live payload + its current peak.
    let mut bars = [0.0f32; MAX_BANDS];
    let mut curve = [0.0f32; VIZ_CURVE];
    let (values, count): (&[f32], usize) = if spectral {
        state.shared.viz.read_curve(&mut curve);
        (&curve[..], VIZ_CURVE)
    } else {
        let active = state.shared.viz.read_bars(&mut bars);
        (&bars[..active.max(1)], active.max(1))
    };
    let cur_max = values.iter().cloned().fold(0.0f32, f32::max);

    // Stable auto-gain: jump up instantly, decay slowly so bars don't flicker.
    if cur_max > state.viz_peak {
        state.viz_peak = cur_max;
    } else {
        state.viz_peak = state.viz_peak * 0.94 + cur_max * 0.06;
    }
    let norm = state.viz_peak.max(1e-4);

    let pad = 6.0;
    let plot = egui::Rect::from_min_max(
        rect.min + egui::vec2(pad, pad + 10.0),
        rect.max - egui::vec2(pad, pad),
    );
    let low = egui::Color32::from_rgb(56, 132, 160); // teal (quiet)
    let high = egui::Color32::from_rgb(244, 182, 66); // amber (loud)
    let lerp = |a: egui::Color32, b: egui::Color32, t: f32| {
        let t = t.clamp(0.0, 1.0);
        egui::Color32::from_rgb(
            (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
            (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
            (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
        )
    };

    let n = count.max(1);
    let slot_w = plot.width() / n as f32;
    let gap = if spectral { 0.0 } else { (slot_w * 0.18).min(3.0) };
    let mut tops: Vec<egui::Pos2> = Vec::with_capacity(n);
    for (i, &v) in values.iter().take(n).enumerate() {
        let t = (v / norm).clamp(0.0, 1.0);
        let h = t * plot.height();
        let x0 = plot.left() + i as f32 * slot_w + gap * 0.5;
        let x1 = plot.left() + (i as f32 + 1.0) * slot_w - gap * 0.5;
        let y1 = plot.bottom();
        let y0 = y1 - h.max(1.0);
        let col = lerp(low, high, t);
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1)),
            if spectral { 0.0 } else { 1.5 },
            col,
        );
        tops.push(egui::pos2((x0 + x1) * 0.5, y0));
    }

    // Spectral: overlay the envelope outline so the fill reads as a curve.
    if spectral && tops.len() > 1 {
        painter.add(egui::Shape::line(
            tops,
            egui::Stroke::new(1.5, egui::Color32::from_rgb(250, 214, 130)),
        ));
    }

    let label = if spectral { "formant envelope" } else { "band activity" };
    painter.text(
        rect.min + egui::vec2(8.0, 6.0),
        egui::Align2::LEFT_TOP,
        label,
        egui::FontId::proportional(11.0),
        egui::Color32::from_gray(150),
    );
}

fn param(ui: &mut egui::Ui, state: &GuiState, idx: usize) {
    core_gui::dirty_param_row_g(
        ui,
        &state.shared.params[idx],
        &PARAMS[idx],
        &state.shared.dirty_params[idx],
        core_gui::GestureBridge {
            begin: &state.shared.gesture_begin,
            end: &state.shared.gesture_end,
        },
        idx,
    );
}

fn apply_preset(state: &mut GuiState, index: usize) {
    if let Some(preset) = PRESETS.get(index) {
        crate::presets::apply(&state.shared, preset);
        state
            .shared
            .active_preset
            .store(index as u32, std::sync::atomic::Ordering::Relaxed);
    }
}
