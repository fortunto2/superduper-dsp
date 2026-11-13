---
name: sdsp-mash
description: "Build mashups and multi-verse cyphers (one beat plus verses from other songs, or a beat switch per verse) with the sdsp-mash Rust CLI from the superduper-dsp repository. Use when the user says \"make a mashup\", \"cypher\", \"put this vocal on that beat\", \"blend these tracks\", or wants several verses over one beat. Needs a checkout of the repo to build the CLI. Do NOT use for mixing or mastering a single track (sdsp-mix) or for headless plugin chains (sdsp-chain)."
---

# sdsp-mash: mashups and cyphers

> **Needs** a checkout of the [superduper-dsp repository](https://github.com/fortunto2/superduper-dsp), a Rust toolchain, and `ffmpeg`
> for the mp3 step; stems come from `demucs`. This plugin does not include the CLI. Check first:
> `test -f tools/sdsp-mash/Cargo.toml` in the checkout, `cargo --version`, `command -v ffmpeg`. If one is missing, say which and stop
> there rather than improvising. Use only audio you have the right to use; the renders are for personal and educational work.

Engine: `tools/sdsp-mash` in the checkout, run as `cargo run --release -p sdsp-mash -- mash.toml out.wav`.
It follows the pattern of `sdsp-chain`: TOML stages, per-stage LUFS and dBTP printed. `tools/sdsp-mash/README.md` documents the
config; `tools/sdsp-mash/example.toml` is a generic starting point.

## Pipeline

1. **Choose the pair** (the rules decide everything):
   - Tempo: 1:1, 2:1 (half-time), or a stretch within about ±8%. Always stretch the beat, never the vocal: the vocal has to stay recognisable.
   - Key: nearly irrelevant for rap; for sung hooks use neighbours on the Camelot wheel (±1, A↔B).
   - Measure with `sdsp-mash analyze <wav>` (exact BPM, guards against half/double-tempo errors).
2. **Stems**: `uvx --with numpy --from demucs demucs --two-stems=vocals -o OUT "file"` for a vocal; drop `--two-stems` for the full
   4-stem split (drums, bass, other). `--with numpy` is required, otherwise torch fails on numpy. Keep stems outside temp folders:
   configs refer to them by path, and a cleaned scratch directory breaks every old config.
3. **mash.toml**: `bpm` of the grid; `[[track]]` with `role` = beat-drums | beat-bass | beat-other | vocal, `offset_beats`,
   `start_sec` / `len_sec` (cut out a verse), `gain_db`. For a vocal: `highpass_hz` plus a compressor, and `[track.denoise]` first in
   the track chain (mode spectral | dereverb | neural; `learn_s = [start, len]` learns the noise from a stretch without voice).
   `auto_align = true` snaps the vocal to the kick by onset cross-correlation. `[duck]` ducks only `beat-other` (never bass or
   drums, or the low end disappears). `[intro_sweep]` opens a lowpass sweep. Always end with `[[master]]` (eq, compressor, limiter):
   without it a cascade of crossfades piles up level and the mix lands above +3 dBTP.
4. **Render** to WAV, then `ffmpeg -q:a 0` to mp3, and check LUFS and true peak in the printed report.

## Cypher (verses from 2-3 songs)

- Several `[[track]] role = "vocal"` with different `offset_beats` (A at bar 8, B at bar 40, ...), each with its own cut, gain and FX.
- The tempo limit applies per segment when the beat changes between verses.
- Beat-to-beat transitions (`[[section]]` with `transition`): `bass_swap` (8-16 bars of tops, bass swapped in one move),
  `filter_sweep`, `breakdown` (beat drops out for 1-2 bars, vocal alone, new beat hits on the first line), `drop` (build-up
  of snare roll, riser or filter for 4-8 bars, new beat lands on the peak), `cut` (1-2 beats of silence, new beat on the downbeat),
  `fade_except` (everything fades except one element, `keep` = vocal | hats | melody).
- `balance_sections = true` (default) equalises sections by RMS with a ±4.5 dB deadband; sources mastered 15-19 dB apart still need a
  manual `gain_db`.

## Lessons log

The long working log (dated sessions, failures, parameter recipes) is kept in the repo, not in this plugin:
`docs/skill-notes/sdsp-mash-lessons.md` in the checkout (Russian). Read the relevant part before a long session and append new lessons there.
