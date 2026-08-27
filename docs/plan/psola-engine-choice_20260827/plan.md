# Implementation Plan: PSOLA scope guard — pick the pitch engine by material

**Track ID:** psola-engine-choice_20260827
**Spec:** [spec.md](./spec.md)
**Created:** 2026-08-27
**Status:** [ ] Not Started

## Overview

Do not rewrite PSOLA. Measure whether the incoming material has the glottal
epochs PSOLA needs, and route to the phase vocoder when it does not — behind an
`Auto` mode in both plugins, with a fixed worst-case reported latency and a
crossfade on the switch.

## Phase 1: Measure before deciding <!-- checkpoint:f19fcfa -->

Establish the numbers the rest of the track is judged against, and confirm the
descriptor can actually separate the two cases.

### Tasks
- [x] Task 1.1: Extend `effects/superduper-pitch/tests/engine_transparency.rs` into a matrix: {pulsed voice, smooth tone, real vocal take, breathy/noisy take} × {PSOLA, pvoc} × {shift 0, +25 cents, +12 st}, printing noise-to-harmonic for each. Record the table in the test's doc comment as the baseline. <!-- sha:4732255 -->
- [x] Task 1.2: Add `epoch_sharpness(&[f32], t0) -> f32` to `synth-core/src/pitch.rs` (peak-to-RMS within one tracked period, RT-safe, no alloc) plus unit tests in `synth-core/tests/dsp_blocks.rs` asserting it separates the two reference signals with a clear margin. <!-- sha:f19fcfa -->
- [x] Task 1.3: Pick the routing threshold from the measured values (not from taste) and write the chosen number plus its evidence into the `epoch_sharpness` doc comment. <!-- sha:f19fcfa -->

### Verification
- [x] `cargo test --release -p superduper-pitch --test engine_transparency -- --nocapture` prints the full matrix
- [x] The descriptor's margin between pulsed and smooth is at least 2× on the reference signals (measured 5.8×: 2.65 vs 0.46)

## Phase 2: Engine router in synth-core <!-- checkpoint:ea42790 -->

One place decides which engine runs, so both plugins get identical behaviour.

### Tasks
- [x] Task 2.1: Move the phase vocoder from `effects/superduper-pitch/src/pvoc.rs` into `synth-core/src/pvoc.rs`, re-exporting it from the plugin under the old path (the `wave_osc` precedent in synth-core/CLAUDE.md) so both plugins — and iOS — can reach it. <!-- sha:30fd30d -->
- [x] Task 2.2: Add `synth-core/src/pitch_engine.rs`: a `PitchEngine` wrapper owning both a `PitchShifter` and a `PhaseVocoder`, with `Mode { Psola, Pvoc, Auto }`, one `process()` signature, a latency reported as the max of both engines (fixed at construction), and an equal-power crossfade over ~20 ms when Auto switches. <!-- sha:ea42790 -->
- [x] Task 2.3: Derive the PSOLA pitch floor from the tracked f0 instead of the 95 Hz default — pass a real floor into `PitchShifter::with_range` so an 87 Hz voice locks; keep the reported latency fixed regardless. <!-- sha:ea42790 -->
- [x] Task 2.4: Add `synth-core/tests/pitch_engine.rs`: Auto picks PSOLA on the pulsed source and pvoc on the smooth one; a forced mid-note switch stays under the `click_audit` step bound; unity shift is within 2 dB of the input on both sources. <!-- sha:ea42790 -->

### Verification
- [x] `cargo test --release -p superduper-synth-core` green (93: 58 + 26 + 8 pitch_engine + 1)
- [x] Allocation-free: the sdsp-test-kit counting allocator reports zero allocations in `process()` (`process_does_not_allocate` green in both plugins)

## Phase 3: Wire the plugins <!-- checkpoint:a877cc7 -->

### Tasks
- [x] Task 3.1: `superduper-pitch` — swap its two engine fields for `PitchEngine`, extend the `Mode` param to `Voice | Track | Auto` (append the new value, keep 0/1 as they are), update `value_to_text` and `gui.rs`'s mode row, and re-record `tests/quality.snap` with `SDSP_UPDATE_SNAPSHOTS=1`. <!-- sha:d9d83c2 -->
- [x] Task 3.2: `superduper-tune` — replace its direct `PitchShifter` with `PitchEngine` in `src/dsp.rs`, append an `Engine` stepped param (`Auto | PSOLA | Phase`) next to the existing `Model` param, and re-record its snapshot. <!-- sha:a12cf79 -->
- [x] Task 3.3: Closed-loop check via `tools/sdsp-tune`-style measurement: correct a smooth synthetic tone and a real vocal take through the Tune plugin (drive it with `sdsp-chain`), re-analyse, and assert median error under 10 cents with no rise in noise floor. <!-- sha:a877cc7 -->

### Verification
- [x] `cargo test --release -p superduper-pitch -p superduper-tune` green (27 + 10)
- [x] `cargo run --release -p sdsp-chain -- --params pitch` / `--params tune` show the new params with correct names (Mode 0..2, Engine 0..2)
- [~] Bundles rebuild and install: `./scripts/build_pitch_bundle.sh && ./scripts/build_tune_bundle.sh` done. **REAPER restart left to the user** — it caches CLAP dylibs per session and restarting would drop an open project.

## Phase 4: Docs & Cleanup

### Tasks
- [x] Task 4.1: Rewrite lesson 24 in `CLAUDE.md` from "open defect" to the shipped rule (Auto routing + the measured numbers), and update the `superduper-pitch` / `superduper-tune` entries with the new params. <!-- sha:369d4ac -->
- [x] Task 4.2: Update `synth-core/CLAUDE.md` for the moved `pvoc` module and the new `pitch_engine`, including the "test do-nothing on both a pulsed and a smooth source" rule. <!-- sha:369d4ac -->
- [x] Task 4.3: Point `tools/sdsp-tune` at `PitchEngine` (it hardcodes pvoc today) and delete the now-duplicated engine comment there. <!-- sha:3c6533e -->

### Verification
- [ ] `cargo test --release --workspace` green
- [ ] `cargo clippy --release -p superduper-synth-core -p superduper-pitch -p superduper-tune` clean

## Final Verification
- [ ] All acceptance criteria from spec met
- [ ] Unity-shift transparency within 2 dB of input on both reference sources
- [ ] Tests pass, clippy clean, bundles build
- [ ] CLAUDE.md and synth-core/CLAUDE.md reflect the shipped behaviour

## Context Handoff

### Session Intent
Stop PSOLA from destroying smooth material by detecting the material and
routing to the phase vocoder, instead of rewriting the grain scheduler.

### Key Files
- `synth-core/src/psola.rs` (measured note at the top; `with_range` already added)
- `synth-core/src/pitch.rs` (new `epoch_sharpness`)
- `synth-core/src/pvoc.rs` (moved from `effects/superduper-pitch/src/pvoc.rs`)
- `synth-core/src/pitch_engine.rs` (new)
- `effects/superduper-pitch/src/{lib.rs,dsp.rs,gui.rs}`, `tests/engine_transparency.rs`
- `effects/superduper-tune/src/{lib.rs,dsp.rs,gui.rs}`
- `tools/sdsp-tune/src/main.rs`
- `CLAUDE.md` (lesson 24), `synth-core/CLAUDE.md`

### Decisions Made
- **Route, don't rewrite.** Three grain-scheduler fixes were measured and
  reverted; the engine is correct within its scope, so the cheap and safe move
  is to respect that scope.
- **pvoc moves to synth-core** rather than Tune depending on the Pitch crate —
  effect crates depending on each other would be a new and wrong direction,
  and synth-core is also the only path to iOS.
- **Latency is the max of both engines, fixed at activate.** Hosts mis-handle
  latency that changes at runtime; `with_latency(min_latency)` exists exactly
  for aligning two engines this way.
- **Params are appended, never reordered** (lesson 10 — REAPER caches param
  layouts per plugin id and FX slot).

### Risks
- A crossfade between two engines with different latencies can smear a
  transient if the alignment is wrong; verify against `click_audit` and by ear
  on a percussive take.
- `epoch_sharpness` may straddle the threshold on breathy singing, causing
  repeated switching — add hysteresis and require the decision to hold for
  several analyses before acting (the same "stubbornness budget" pattern the
  pitch tracker's candidate scoring uses).
- Moving `pvoc` touches `keydetect`/Track-mode call sites in superduper-pitch;
  keep the re-export so nothing outside the crate needs editing.
- `tools/sdsp-tune`'s numbers are the regression baseline for offline quality —
  re-run its closed loop before and after the switch to `PitchEngine`.

---
_Generated by /plan. Tasks marked [~] in progress and [x] complete by /build._
