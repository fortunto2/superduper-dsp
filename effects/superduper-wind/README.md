# SuperDuper Wind

Breath/wind instrument — kurai / nay / low Bashkir flute, **or actual
howling wind**. Built on Spectral Modeling Synthesis (SMS): a deterministic
additive tone plus a stochastic noise "wind bed" that cross-fades between a
gentle formant-shaped breath layer and a procedural howling-wind engine.

| | |
|---|---|
| Category | Instrument + Effect hybrid (note-in AND audio-in → audio-out) |
| Stereo | yes (decorrelated L/R noise) |
| Latency | 0 samples |
| Voices | 8 (Instrument mode) |

## Engine

- **Deterministic tone** — 6 additive harmonics (fundamental + 5
  overtones), brightness set by `Tone` (steep 1/n rolloff = dark, gentle
  rolloff = bright), shaped by a 3-band `Formant` filter (base F1/F2/F3 =
  500/1100/2000 Hz, multiplied by `Formant` in semitones). Fades down as
  `Howl` rises so a full-howl patch reads as wind, not flute.
- **Stochastic wind bed — two engines cross-faded by `Howl`:**
  - *Gentle breath* (`Howl` → 0) — noise blended between pink/dark
    (`Color` = 0) and white/airy (`Color` = 1), bandpassed at the same
    formant bands as the tone. Pink noise is Paul Kellet's cheap 3-pole
    1/f cascade.
  - *Procedural howling wind* (`Howl` → 1) — Andy Farnell's "Designing
    Sound" wind model: broadband noise through **3 high-Q resonant
    bandpasses**, each independently swept by an LFO + a smoothed random
    walk (0.1-2 Hz) across **~200 Hz-2 kHz** — the pitched "whoooo" of
    real wind («завывание»). The played note transposes the sweep range,
    so it's still a playable instrument, not a fixed sample.
  - Amplitude = `Breath` × note-envelope × (1 + `Shimmer` wobble) ×
    the shared `Gust` surge multiplier.
- **Gust** — ONE shared slow (0.05-0.5 Hz) surge envelope (not per-voice —
  a real gust hits every held note together) that swells the whole wind
  bed's amplitude, giving the howling its characteristic surging quality.
- **Jitter / Shimmer** — smoothed 1/f noise (pink noise through an extra
  one-pole lowpass) driving pitch wobble (±40 cents at Jitter=1) and
  wind-bed amplitude wobble — organic, non-repeating, unlike an LFO.
- **Chiff** — a short (~50 ms) broadband breath-noise burst on note-on,
  the "tongued attack" of a real wind instrument.
- Per-channel noise is fully decorrelated (independent PRNG per L/R) so
  the wind bed has real stereo width, not a mono-duplicated hiss.

## Mode — one plugin, two personalities

- **Instrument** — an 8-voice polyphonic note-driven synth (click-free
  deferred-steal voice pool, legato retrigger, choke-fade on steal).
- **Overlay** — ignores notes and instead reads the track's existing
  audio, doing TWO coupled things so the effect is unmistakable on an
  insert:
  1. A wind-bed keyed to the input's envelope (`EnvelopeDetector`),
     swelling further with `Gust`, loosely following the input's pitch
     (`YinPitchTracker`).
  2. The SAME `Gust` envelope opens/closes a **resonant lowpass directly
     on the dry input** (fully open ~17 kHz → muffled ~500 Hz at full
     gust) — a real sidechain-style "the wind blows through it" duck, not
     just an added layer.

## Parameters

| Group | Params |
|---|---|
| Mode | Mode (Instrument / Overlay) |
| Tone | Tone, Formant |
| Wind | Breath, Jitter, Shimmer, Chiff, Color, Howl, Gust |
| Envelope | Attack, Release |
| Output | Mix (Overlay wet), Output, Bend Range |
| Preset | Preset (stepped, host/MCP recallable) |

## Presets

- **Kurai (Low Wind)** — dark, breathy, low formants, soft slow attack,
  low Howl (gentle breath character). The flagship patch and the default
  landing preset.
- **Nay** — brighter reed-flute character, faster attack, minimal howl.
- **Wind Pad** — atmospheric drone, max jitter/shimmer, long
  attack/release, partial howl + strong gusts.
- **Wind (Howl)** — the procedural howling-wind engine dominant: near-
  silent tone, max Breath + Howl, strong gusts. Actual howling wind,
  still playable by MIDI note.
- **Air Enhancer** (Overlay mode) — wind-bed + obvious gust-ducking on
  top of whatever is already on the track.
- **Init** — PARAMS defaults, a neutral reset target.

## Workflow

1. Pick **Kurai** for a dark held-note drone, **Nay** for a brighter
   melodic line, **Wind Pad** for an atmospheric bed, or **Wind (Howl)**
   for actual howling wind.
2. Dial `Breath` for how much air/wind vs. tone you hear; `Howl` for
   gentle breath vs. dramatic procedural howl; `Color` for dark/pink vs.
   white/airy noise character; `Gust` for how hard it surges.
3. `Jitter`/`Shimmer` add human imperfection — small amounts (0.1-0.3)
   read as "alive", large amounts (0.7+) read as unstable/atmospheric.
4. Switch `Mode` to **Overlay** and pick **Air Enhancer** to add a
   gust-driven wind bed — and duck the track's own tone — on top of an
   existing vocal, lead, or bass instead of playing new notes.
