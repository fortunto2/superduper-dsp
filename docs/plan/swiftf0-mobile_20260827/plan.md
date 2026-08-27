# Implementation Plan: SwiftF0 on iPhone — neural pitch for live2play

**Track ID:** swiftf0-mobile_20260827
**Spec:** [spec.md](./spec.md)
**Created:** 2026-08-27
**Status:** [ ] Not Started

## Overview

The Rust tracker already cross-compiles to iOS through synth-core, so wire it
up first and measure it on a real phone. Build the Core ML / ANE path only if
the measurement says the CPU cost is too high — the decision is a number, not
a preference.

## Phase 1: Rust tracker on the phone

### Tasks
- [ ] Task 1.1: Add a pitch C ABI to `mobile/sdsp-ios/src/lib.rs`: create/destroy a tracker, push a block, read Hz + confidence, and a switch between YIN (`synth_core::pitch`) and SwiftF0 (`synth_core::swiftf0`) so the fallback is one call away.
- [ ] Task 1.2: Rebuild the framework (`make ios`) and confirm the binary size delta from the 379 KB weight blob is what's expected — record the before/after numbers.
- [ ] Task 1.3: Add a `/pitch` debug endpoint on the reelcam side (same pattern as the existing `:9010 /gesture`) reporting current Hz, confidence, and per-block CPU time, so every later measurement is one `curl` from the Mac.

### Verification
- [ ] `make ios` succeeds and the app links
- [ ] Singing into the phone moves the reported Hz sensibly, gated by confidence

## Phase 2: Measure on the device

The whole track turns on these numbers; take them before writing any more code.

### Tasks
- [ ] Task 2.1: Measure both trackers on a real iPhone via `/pitch`: CPU per second of audio, worst-case block time, and the remaining audio-thread headroom — interleaving the two variants within each round (never one after the other, per the model-shrink measurement rules).
- [ ] Task 2.2: Run a 10-minute sustained test and watch the existing perf meter (DET/VIS/LAT/BURN/DROP) for thermal drift, comparing against the baseline in `reference_reelcam_perf_pipeline`.
- [ ] Task 2.3: Write the comparison table (YIN vs SwiftF0-Rust, and the M1 Core ML figures already recorded) into `mobile/sdsp-ios`'s docs with an explicit recommendation.

### Verification
- [ ] Numbers exist for both trackers from the device, not the simulator
- [ ] The recommendation is stated with its evidence

## Phase 3: Core ML path — only if Phase 2 demands it

_Skip this phase entirely if the Rust tracker fits the budget. Building ANE
plumbing that nothing needs is the failure mode to avoid._

### Tasks
- [ ] Task 3.1: Ship `~/Music/1music/swiftf0-coreml/SwiftF0.mlpackage` as a download-on-demand asset (download → `MLModel.compileModel` → Application Support, excluded from backups, validated by loading), following the delivery pattern in the `model-shrink` skill.
- [ ] Task 3.2: Implement the STFT front end on the device with vDSP to the exact contract in that folder's README (16 kHz, pad 384, frame 1024 / hop 256, the model's own window, bins 3..135, ln(x + 1e-8), layout [1,132,T]) and assert it matches the Rust implementation's features on one shared test vector.
- [ ] Task 3.3: Run inference off the audio thread, publish results through a lock-free slot, and measure the added latency; re-run the Phase 2 comparison with this path included.

### Verification
- [ ] Feature vectors from vDSP match the Rust path within float tolerance
- [ ] The audio thread never blocks on `predict`
- [ ] The ANE path beats the Rust path on the device by a margin worth its complexity — otherwise revert to Phase 1's path and record why

## Phase 4: Wire into live2play + Docs

### Tasks
- [ ] Task 4.1: Route the chosen tracker into the voice→instrument mapping, with SwiftF0's confidence as the voiced gate (YIN holds its last value instead, so the mapping must handle both shapes).
- [ ] Task 4.2: Expose the tracker choice as a setting in the app, defaulting to whatever Phase 2 recommended, with YIN always available as the cheap fallback.
- [ ] Task 4.3: Add the CC-BY-4.0 attribution for SwiftF0 to the app's credits, and update `synth-core/CLAUDE.md` + the reelcam CLAUDE.md with the shipped setup and the device numbers.

### Verification
- [ ] Singing drives the instrument through the neural tracker on the device
- [ ] Switching to YIN at runtime works and costs less CPU
- [ ] Attribution present

## Final Verification
- [ ] All acceptance criteria from spec met
- [ ] Device numbers recorded for every path built
- [ ] `make ios` clean, app runs, no thermal regression over 10 minutes
- [ ] Docs updated on both the DSP and the app side

## Context Handoff

### Session Intent
Get the neural pitch tracker onto the phone for live2play, choosing between
the Rust and Core ML paths by measurement on a real device.

### Key Files
- `mobile/sdsp-ios/src/lib.rs`, `mobile/sdsp-ios/build-xcframework.sh`, `Makefile` (`make ios`)
- `synth-core/src/swiftf0.rs`, `synth-core/src/pitch.rs`
- `~/Music/1music/swiftf0-coreml/` (mlpackage + README recipe)
- reelcam app: debug endpoint, gesture→instrument mapping, credits
- `synth-core/CLAUDE.md`, reelcam `CLAUDE.md`

### Decisions Made
- **Rust first.** synth-core already reaches iOS, so the cheap experiment
  comes before the expensive one.
- **The ANE is not free.** Core ML runs off the audio thread, so it buys
  throughput at the cost of a block of latency and real plumbing — worth it
  only if the CPU path misses the budget.
- **YIN stays.** It is 10× cheaper and exact on clean, close-mic'd input; the
  neural tracker earns its place on noisy or distant material.

### Risks
- Battery and thermals matter more than peak throughput on a phone; the
  10-minute sustained test is the real gate, not the per-block number.
- The 379 KB weight blob is linked into the binary — fine now, but if more
  models follow, move to download-on-demand for all of them at once.
- Simulator numbers are meaningless here (no hardware codecs, different
  scheduler) — every figure must come from the device.
- Measuring two variants sequentially invents speedups from machine-load
  drift; interleave them.

---
_Generated by /plan. Tasks marked [~] in progress and [x] complete by /build._
