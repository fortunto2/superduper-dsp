//! egui_baseview GUI for SuperDuper Wind.
//!
//! Per-plugin specifics only — window sizing, section layout, preset list.
//! Shared style/layout primitives come from `superduper_synth_core::gui` so
//! Wind picks up the same look as every other SuperDuper plugin.

use baseview::{PhySize, Size, WindowHandle, WindowOpenOptions, WindowScalePolicy};
use egui_baseview::{EguiWindow, GraphicsConfig};
use raw_window_handle::HasRawWindowHandle;
use std::sync::atomic::Ordering;
use superduper_synth_core::gui as core_gui;

use crate::presets::PRESETS;
use crate::{
    apply_preset_idx, P_ATTACK, P_BEND_RANGE, P_BREATH, P_CHIFF, P_COLOR, P_FORMANT, P_GUST,
    P_HOWL, P_JITTER, P_MIX, P_MODE, P_OUTPUT, P_RELEASE, P_SHIMMER, P_TONE, P_WHISTLE, PARAMS,
    SharedParams,
};

pub const DEFAULT_WIDTH: u32 = 560;
pub const DEFAULT_HEIGHT: u32 = 680;
pub const MIN_WIDTH: u32 = 420;
pub const MIN_HEIGHT: u32 = 500;
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

/// Shorthand for the `dirty_param_row_g` call — every row needs the same
/// atom/def/dirty/gesture quadruple, this just saves repeating `state.shared`
/// four times per parameter.
fn row(ui: &mut egui::Ui, state: &GuiState, idx: usize) {
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

pub fn open_window<P: HasRawWindowHandle>(
    parent: &P,
    shared: SharedParams,
    resize: ResizeBridge,
) -> WindowHandle {
    let (initial_w, initial_h) = core_gui::read_bridge(&resize);
    let settings = WindowOpenOptions {
        title: "SuperDuper Wind".to_string(),
        size: Size::new(initial_w as f64, initial_h as f64),
        scale: WindowScalePolicy::SystemScaleFactor,
        gl_config: Some(Default::default()),
    };
    let initial_preset_idx = (shared.active_preset.load(Ordering::Relaxed) as usize)
        .min(PRESETS.len().saturating_sub(1));
    let state = GuiState {
        shared,
        resize,
        applied_size: (initial_w, initial_h),
        selected_preset: Some(initial_preset_idx),
        preset_names: PRESETS.iter().map(|p| p.name).collect(),
    };

    EguiWindow::open_parented(
        parent,
        settings,
        GraphicsConfig::default(),
        state,
        |ctx: &egui::Context, _queue, _state: &mut GuiState| core_gui::install_default_style(ctx),
        |ctx: &egui::Context, queue, state: &mut GuiState| {
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
            "SuperDuper Wind",
            env!("SDSP_BUILD_NUM"),
            env!("SDSP_BUILD_DATE"),
            &state.shared.bypass,
            "wind_preset_combo",
            &state.preset_names,
            &mut state.selected_preset,
        ) {
            apply_preset_idx(&state.shared, i);
        }

        core_gui::ab_init_bar(
            ui,
            &state.shared.ab_snapshot,
            &state.shared.params,
            PARAMS,
            &state.shared.dirty_params,
        );

        let (scope_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 70.0),
            egui::Sense::hover(),
        );
        core_gui::draw_spectrum_strip(ui, &state.shared.scope, scope_rect, 48_000.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            core_gui::section(ui, "Mode", |ui| {
                let on_overlay = state.shared.params[P_MODE].load(Ordering::Relaxed) >= 0.5;
                core_gui::dirty_choice_row_g(
                    ui,
                    &state.shared.params[P_MODE],
                    &PARAMS[P_MODE],
                    &["Instrument", "Overlay"],
                    &state.shared.dirty_params[P_MODE],
                    core_gui::GestureBridge {
                        begin: &state.shared.gesture_begin,
                        end: &state.shared.gesture_end,
                    },
                    P_MODE,
                );
                ui.label(
                    egui::RichText::new(if on_overlay {
                        "Overlay reads the track's audio and layers breath on top."
                    } else {
                        "Instrument plays notes — an 8-voice breath synth."
                    })
                    .weak()
                    .small(),
                );
            });

            core_gui::section(ui, "Tone", |ui| {
                row(ui, state, P_TONE);
                row(ui, state, P_FORMANT);
            });

            core_gui::section(ui, "Wind", |ui| {
                row(ui, state, P_BREATH);
                row(ui, state, P_JITTER);
                row(ui, state, P_SHIMMER);
                row(ui, state, P_CHIFF);
                row(ui, state, P_COLOR);
                row(ui, state, P_HOWL);
                row(ui, state, P_GUST);
                row(ui, state, P_WHISTLE);
            });

            core_gui::section(ui, "Envelope", |ui| {
                row(ui, state, P_ATTACK);
                row(ui, state, P_RELEASE);
            });

            core_gui::section(ui, "Output", |ui| {
                row(ui, state, P_MIX);
                row(ui, state, P_OUTPUT);
                row(ui, state, P_BEND_RANGE);
            });

            core_gui::help_block(
                ui,
                "wind_help",
                &[
                    (
                        "Spectral Modeling Synthesis",
                        "Wind splits every sound into a deterministic tone (a few additive \
                         harmonics, brightness = Tone) and a stochastic noise \"wind bed\". \
                         Breath sets how much air/wind you hear; Color blends the noise \
                         between dark/pink (wind-like) and white (airy hiss).",
                    ),
                    (
                        "Howl — gentle breath ↔ actual howling wind",
                        "The wind bed is a cross-fade of two noise engines. At Howl=0 it's the \
                         original gentle formant-bandpassed breath (gets you Kurai/Nay's airy \
                         flute character). As Howl rises it morphs into a procedural HOWLING \
                         WIND model (Farnell \"Designing Sound\"): broadband noise through 2-3 \
                         high-Q resonant bandpasses swept by independent LFOs + a random walk \
                         across ~200 Hz-2 kHz — the pitched \"whoooo\" of real wind. The played \
                         note transposes the sweep range, so it's still playable. High Howl \
                         also fades the additive tone down so the patch reads as wind, not \
                         flute — see the Wind (Howl) preset.",
                    ),
                    (
                        "Gust",
                        "A slow (0.05-0.5 Hz) shared surge envelope that swells the whole wind \
                         bed uniformly — one gust affects every held note together, not each \
                         independently. In Overlay mode the SAME gust also closes/opens a \
                         resonant lowpass on the dry input, so the track audibly darkens as the \
                         wind surges and brightens again as it recedes.",
                    ),
                    (
                        "Whistle — Aeolian tone",
                        "Adds the tonal whistle you hear when wind passes a wire or edge — a \
                         real vortex-shedding (Aeolian) tone. Frequency comes from the Strouhal \
                         relation f = St·U/d (St≈0.2): as Gust drives the virtual wind speed U \
                         up, the whistle glides UP in pitch and gets louder, then falls back as \
                         the gust recedes. Gated by Howl (no whistle in the gentle-breath end of \
                         the range) and transposed by the played note. Try Howling Gale.",
                    ),
                    (
                        "Jitter / Shimmer / Chiff",
                        "Jitter wobbles pitch, Shimmer wobbles the wind bed's amplitude — both \
                         driven by smoothed 1/f noise for an organic, non-repeating wander. \
                         Chiff adds a short noise burst on note-on, like a tongued attack.",
                    ),
                    (
                        "Instrument vs Overlay",
                        "Instrument is a normal 8-voice poly synth — play it from a MIDI \
                         track. Overlay ignores notes and instead reads whatever audio is \
                         already on the track, tracks its envelope, and layers the same wind \
                         engine on top (keyed to that envelope) scaled by Mix, while Gust ducks \
                         the dry signal through the sweeping lowpass — an obvious, sidechain- \
                         style \"wind blowing over the track\" effect even at Mix=0.5.",
                    ),
                ],
            );
        });
    });
}
