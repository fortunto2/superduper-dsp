# Specification: Melody editor — standalone Rust GUI over the note model

**Track ID:** melody-editor_20260827
**Type:** Feature
**Created:** 2026-08-27
**Status:** Draft

## Summary

`tools/sdsp-tune` already does offline melody correction: it segments a take
into notes (`synth_core::melody`), retunes each note as a whole (median →
target, vibrato untouched), renders through the phase vocoder, and writes the
note list as an **editable text file** plus a PNG of the melody. The text file
is the manual editing surface today: change a `target` column, re-run with
`--apply`, and only that note moves.

This track puts a real editor on top of that model — drag a note, hear it,
save. Deliberately a **standalone app**, not a plugin: a plugin receives audio
in blocks and never sees the whole clip, and Melodyne-style editing inside a
DAW requires ARA2, which does not exist for CLAP. Standalone is the only
honest path, and it costs nothing extra because the model, the renderer and
the picture already exist.

Stack: egui (already used by all 31 plugins at 0.33) with `eframe` for the
window instead of `egui-baseview`, plus `cpal` for audition (already a
dependency of `tools/sdsp-runner`).

## Acceptance Criteria

- [ ] Opens a WAV, shows the pitch curve, note blocks and targets on a
      pan/zoom timeline (the same information the PNG shows today)
- [ ] A note can be dragged vertically to a new target, snapping to scale
      degrees, with the correction rendered and audible without leaving the app
- [ ] A note can be marked "leave as sung" (the `-` target in the text format)
- [ ] Per-note vibrato flatten and correction amount are editable, not just
      global
- [ ] Play / stop with a moving cursor, A/B between original and corrected
- [ ] Save writes the corrected WAV; the note list round-trips through the
      existing text format so CLI and GUI stay interchangeable
- [ ] Note segmentation parameters (split cents, min duration, confidence
      gate) are adjustable and re-segment live
- [ ] Runs on macOS arm64 from `cargo run --release -p sdsp-melody`

## Dependencies

- `synth_core::melody` — the note model (segment / plan_targets / shift_curve /
  notes_to_text / parse_notes), already shipped
- `synth_core::swiftf0` — the analysis tracker used offline
- The phase vocoder (currently `superduper_pitch::pvoc`; the
  **psola-engine-choice** track moves it to synth-core — prefer landing that
  first, or depend on the plugin crate temporarily)
- `eframe` 0.33 (matching the workspace egui), `cpal` 0.15, `hound` 3.5

## Out of Scope

- Polyphonic editing (the model is monophonic; Melodyne's DNA algorithm is out
  of reach and out of need here)
- Time editing — moving a note's start/end, quantising rhythm (pitch only for
  M1; the model has the frame indices to add it later)
- Formant / timbre editing per note
- A plugin or ARA integration
- Windows / Linux builds (macOS first; nothing in the stack blocks them later)

## Technical Notes

- The model already carries everything the UI needs per note: `start_s`,
  `end_s`, `first`/`last` frame indices, `sung_st`, `target_st`,
  `range_cents`. A UI edit is a write to `target_st` — no new data model.
- Rendering is `melody::shift_curve` → the vocoder, exactly as
  `tools/sdsp-tune::render` does it. Reuse that function rather than writing a
  second renderer (the `sdsp-chain` lesson: "don't write a second renderer").
- **Latency compensation matters**: the engine emits the past, so the shift
  curve must be looked up at `t − latency`. `sdsp-tune` gets this right after
  measuring it wrong (median error stayed 20 cents until fixed) — copy the
  working code, not the idea.
- Re-rendering the whole file on every drag is too slow to feel live. Render
  the edited note's span plus a lead-in of `latency + one note` and splice, or
  render in a background thread and swap buffers. Measure before optimising:
  the current renderer does 30 s in ~2 s.
- The PNG drawing code in `tools/sdsp-tune/src/main.rs` (`Canvas`, `draw`) is a
  minimal software renderer for the same layout. It is the reference for the
  view transform (`xs`/`ys`), but the GUI should draw with egui's painter, and
  the PNG path stays for headless use.
- Audition: `cpal` output stream reading from a shared buffer, the same pattern
  as `tools/sdsp-runner`.
- Keep the app in `tools/sdsp-melody/` and register it in the workspace
  members list, alongside the other tools.

## Related work

- `docs/plan/psola-engine-choice_20260827/` — moves `pvoc` into synth-core,
  which this track wants; land it first if convenient.
- The Melodyne-lite follow-on is recorded as M4 in the `superduper-tune`
  section of `CLAUDE.md`; this track is that item, minus the live half.
