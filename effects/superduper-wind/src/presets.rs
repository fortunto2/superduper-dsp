//! Built-in presets for SuperDuper Wind.
//!
//! Every preset is a full `[f32; PARAMS.len()]` keyed by the `P_*` index
//! constants from `lib.rs` — `define_preset!` gives us the typo-proof sparse
//! `from_overrides` constructor every other simple-state plugin uses
//! (reverb/saturator/…). Applying a preset writes those values straight
//! into the shared atomics: the audio thread picks them up next sample,
//! the host's slider position updates on its next `get_value` query.

use crate::{
    P_ATTACK, P_BREATH, P_CHIFF, P_COLOR, P_FORMANT, P_GUST, P_HOWL, P_JITTER, P_MIX, P_MODE,
    P_OUTPUT, P_RELEASE, P_SHIMMER, P_TONE, P_WHISTLE, PARAMS,
};

superduper_dsp_sdk::define_preset!(PARAMS);

pub const MODE_INSTRUMENT: f32 = 0.0;
pub const MODE_OVERLAY: f32 = 1.0;

/// Catalog of factory presets shown in the GUI dropdown AND recallable via
/// the stepped `Preset` CLAP param (`P_PRESET`, last in `PARAMS`).
/// **Order is frozen once shipped** — `Preset` recall is index-based, so
/// reordering silently renames every host automation lane pointing at an
/// index. Append new presets at the end.
pub static PRESETS: &[Preset] = &[
    // Straight off the PARAMS defaults — a neutral landing pad + reset target.
    Preset::from_overrides("Init", &[]),

    // Kurai (Low Wind) — dark Bashkir kurai / low nay character: heavy
    // breath, low formants (Formant shifted well below the 500/1100/2000 Hz
    // base), soft slow attack, long release, mostly tonal (Howl kept low —
    // this is the gentle breath-flute character, not the howling engine).
    Preset::from_overrides("Kurai (Low Wind)", &[
        (P_MODE, MODE_INSTRUMENT),
        (P_BREATH, 0.75),
        (P_JITTER, 0.28),
        (P_SHIMMER, 0.22),
        (P_TONE, 0.15),
        (P_FORMANT, -4.0),
        (P_ATTACK, 180.0),
        (P_RELEASE, 500.0),
        (P_CHIFF, 0.35),
        (P_COLOR, 0.2),
        (P_HOWL, 0.15),
        (P_GUST, 0.2),
        (P_OUTPUT, -2.0),
    ]),

    // Nay — brighter reed-flute character: higher formants, faster attack,
    // moderate breath so the tone still cuts through. Almost no howl.
    Preset::from_overrides("Nay", &[
        (P_MODE, MODE_INSTRUMENT),
        (P_BREATH, 0.45),
        (P_JITTER, 0.15),
        (P_SHIMMER, 0.15),
        (P_TONE, 0.65),
        (P_FORMANT, 5.0),
        (P_ATTACK, 40.0),
        (P_RELEASE, 220.0),
        (P_CHIFF, 0.4),
        (P_COLOR, 0.55),
        (P_HOWL, 0.08),
        (P_GUST, 0.1),
    ]),

    // Wind Pad — atmospheric drone: maximum jitter/shimmer wander, very
    // long attack/release, dim tone, partial howl blend + strong gusts so
    // it genuinely surges like weather rather than just a static pad.
    Preset::from_overrides("Wind Pad", &[
        (P_MODE, MODE_INSTRUMENT),
        (P_BREATH, 0.6),
        (P_JITTER, 0.9),
        (P_SHIMMER, 0.85),
        (P_TONE, 0.3),
        (P_FORMANT, -1.0),
        (P_ATTACK, 1200.0),
        (P_RELEASE, 2500.0),
        (P_CHIFF, 0.05),
        (P_COLOR, 0.3),
        (P_HOWL, 0.45),
        (P_GUST, 0.7),
    ]),

    // Wind (Howl) — the procedural Farnell howling-wind engine dominant:
    // near-silent additive tone, max Breath + Howl (tight, widely-swept
    // resonant bands), strong gusts, soft attack, a touch of Aeolian
    // whistle riding the gusts. Still a MIDI instrument — the played note
    // transposes both the howl's sweep range and the whistle (`voice.rs`).
    Preset::from_overrides("Wind (Howl)", &[
        (P_MODE, MODE_INSTRUMENT),
        (P_BREATH, 0.95),
        (P_JITTER, 0.4),
        (P_SHIMMER, 0.5),
        (P_TONE, 0.1),
        (P_FORMANT, -6.0),
        (P_ATTACK, 400.0),
        (P_RELEASE, 900.0),
        (P_CHIFF, 0.1),
        (P_COLOR, 0.35),
        (P_HOWL, 0.95),
        (P_GUST, 0.65),
        (P_WHISTLE, 0.6),
        (P_OUTPUT, -3.0),
    ]),

    // Air Enhancer — Overlay mode: layers a howling wind-bed on top of
    // whatever audio is already on the track (vocal, lead, pad, bass),
    // keyed to its envelope, with gusts that audibly duck/filter the
    // input too — an unmistakable "wind is happening on this track" effect.
    Preset::from_overrides("Air Enhancer", &[
        (P_MODE, MODE_OVERLAY),
        (P_BREATH, 0.6),
        (P_MIX, 0.5),
        (P_COLOR, 0.6),
        (P_HOWL, 0.55),
        (P_GUST, 0.6),
        (P_FORMANT, 2.0),
    ]),

    // Howling Gale — the Aeolian-tone whistle showcase: max Howl + Whistle
    // + Gust, so the vortex-shedding tone glides audibly up in pitch and
    // amplitude on every gust surge (Strouhal `f = St·U/d`) on top of a
    // near-maximal broadband howl. Deliberately more extreme than Wind
    // (Howl) — this is "storm", not "breeze".
    Preset::from_overrides("Howling Gale", &[
        (P_MODE, MODE_INSTRUMENT),
        (P_BREATH, 1.0),
        (P_JITTER, 0.5),
        (P_SHIMMER, 0.6),
        (P_TONE, 0.05),
        (P_FORMANT, -8.0),
        (P_ATTACK, 250.0),
        (P_RELEASE, 1100.0),
        (P_CHIFF, 0.15),
        (P_COLOR, 0.3),
        (P_HOWL, 1.0),
        (P_GUST, 0.8),
        (P_WHISTLE, 0.9),
        (P_OUTPUT, -4.0),
    ]),
];

/// Kept separate from `PRESETS.len()` on purpose — referencing `PRESETS`
/// (whose element type embeds `PARAMS.len()`) from inside `PARAMS`'s own
/// `Preset` param range would be a const-eval cycle (E0391). See CLAUDE.md
/// "Gotcha" note under the Preset-selector cross-cutting feature.
pub const PRESET_COUNT: usize = 7;
const _: () = assert!(PRESET_COUNT == PRESETS.len(), "PRESET_COUNT out of sync with PRESETS");

/// Index of the preset a fresh plugin instance boots into — Kurai is the
/// most representative "wind instrument" tone, matching the Kubyz/Wave
/// convention of not landing on the (relatively boring) Init patch.
pub const DEFAULT_PRESET: usize = 1;
