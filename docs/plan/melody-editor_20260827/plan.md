# Implementation Plan: Melody editor — standalone Rust GUI

**Track ID:** melody-editor_20260827
**Spec:** [spec.md](./spec.md)
**Created:** 2026-08-27
**Status:** [ ] Not Started

## Overview

Put an egui window on the note model that `tools/sdsp-tune` already drives:
load → see the melody → drag a note → hear it → save. The model, the renderer
and the layout maths exist; this track is the interaction layer.

## Phase 1: Window that shows the take

A read-only view first — if the picture is wrong, editing on top of it is
wasted work.

### Tasks
- [ ] Task 1.1: Create `tools/sdsp-melody/` (eframe 0.33, cpal 0.15, hound, superduper-synth-core), register it in the workspace `members` list, and open an empty window with a File → Open dialog (`rfd`, already used by superduper-wave's WAV import).
- [ ] Task 1.2: Load a WAV, analyse with `SwiftF0Tracker`, segment with `melody::segment`, and draw the pitch curve, note blocks and target bars on a pan/zoom timeline — port the view transform from `tools/sdsp-tune`'s `draw()` to egui's painter.
- [ ] Task 1.3: Draw the semitone grid with note names, and a readout panel for the selected note (sung / target / error in cents / vibrato range) mirroring the text format's columns.

### Verification
- [ ] The window shows the same melody as `sdsp-tune analyse`'s PNG for the same file
- [ ] Pan and zoom stay smooth on a 3-minute take

## Phase 2: Editing and audition

### Tasks
- [ ] Task 2.1: Select a note; drag it vertically to set `target_st` with snapping to the current key/scale (`melody::nearest_in_scale`), Shift to bypass snapping, and a keystroke to mark it "leave as sung".
- [ ] Task 2.2: Render the correction with `melody::shift_curve` + the phase vocoder, reusing `sdsp-tune`'s `render()` — including its latency-compensated curve lookup — on a background thread, swapping the audition buffer when it completes.
- [ ] Task 2.3: Transport: play / stop / loop-selection over cpal with a moving cursor, and an A/B toggle between original and corrected buffers.
- [ ] Task 2.4: Per-note controls in the side panel: correction amount and vibrato flatten (extend `melody::CorrectOpts` handling so they can vary per note, defaulting to the global value).

### Verification
- [ ] Dragging a note changes its pitch on playback within ~1 s of the drag ending
- [ ] A note marked "leave as sung" measures unchanged after render (re-analyse and compare `sung_st`)
- [ ] A/B is level-matched — no perceived loudness jump when toggling

## Phase 3: Round-trip with the CLI

The GUI and `sdsp-tune` must stay two faces of one model, not two forks.

### Tasks
- [ ] Task 3.1: Save/load the note list through `melody::notes_to_text` / `parse_notes`, so a file edited in the GUI can be re-applied by `sdsp-tune fix --apply` and vice versa; add a round-trip test in `synth-core/tests/` asserting text → notes → text is stable.
- [ ] Task 3.2: Export the corrected WAV (32-bit float, original sample rate and channel count), plus optional export of the note list next to it.
- [ ] Task 3.3: Editable segmentation parameters (split cents, min duration, confidence gate) with live re-segmentation, preserving user-set targets for notes whose time span survives the change.

### Verification
- [ ] A GUI-saved note list drives `sdsp-tune fix --apply` to a bit-identical render
- [ ] Changing segmentation does not silently discard hand-set targets

## Phase 4: Docs & Cleanup

### Tasks
- [ ] Task 4.1: Add a `tools/sdsp-melody` entry to `CLAUDE.md` (what it is, how to run it, that it shares the model with sdsp-tune) and note the ARA/plugin limitation so the "why standalone" question is answered once.
- [ ] Task 4.2: Extend `synth-core/CLAUDE.md`'s `melody` entry with whatever the GUI needed from the model (per-note options, round-trip guarantees).
- [ ] Task 4.3: Remove any drawing/rendering code duplicated from `sdsp-tune` during the port — one implementation, referenced from both.

### Verification
- [ ] `cargo test --release --workspace` green
- [ ] `cargo run --release -p sdsp-melody` opens, loads, edits, saves on a clean checkout

## Final Verification
- [ ] All acceptance criteria from spec met
- [ ] Round-trip CLI ↔ GUI verified on a real take
- [ ] Tests pass, clippy clean
- [ ] CLAUDE.md documents the tool

## Context Handoff

### Session Intent
A standalone Rust app for hand-editing a sung melody, sitting on the note model
that already ships.

### Key Files
- `tools/sdsp-melody/` (new)
- `synth-core/src/melody.rs` — the model (read it first; it defines the whole UI)
- `tools/sdsp-tune/src/main.rs` — `render()`, `draw()`, the option surface to mirror
- `Cargo.toml` workspace members
- `CLAUDE.md`, `synth-core/CLAUDE.md`

### Decisions Made
- **Standalone, not a plugin**: a plugin never sees the whole clip, and
  Melodyne-style editing in a DAW needs ARA2, which CLAP does not have.
- **egui, matching the plugin stack** (0.33) but with `eframe` instead of
  `egui-baseview` — no host window to embed in.
- **The text note list stays the interchange format**, so the CLI keeps
  working and an agent can edit a take without the GUI.

### Risks
- Live re-render latency is the main UX risk; measure before optimising, and
  prefer partial re-render over cleverness.
- egui + eframe 0.33 must not drag a second egui version into the workspace —
  check `cargo tree -d` after adding.
- The phase vocoder currently lives in the superduper-pitch crate; depending on
  a plugin crate from a tool works (`wave-pitch-bench` does it) but the
  psola-engine-choice track moves it to synth-core — prefer that ordering.
- Sample-accurate cursor drawing while cpal runs on its own thread needs a
  shared atomic frame counter; do not lock on the audio thread.

---
_Generated by /plan. Tasks marked [~] in progress and [x] complete by /build._
