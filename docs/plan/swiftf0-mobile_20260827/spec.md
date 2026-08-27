# Specification: SwiftF0 on iPhone — neural pitch for live2play

**Track ID:** swiftf0-mobile_20260827
**Type:** Feature
**Created:** 2026-08-27
**Status:** Draft

## Summary

SwiftF0 (lars76, CC-BY-4.0) now exists in two forms on this machine, both
produced and verified on 2026-08-27:

1. **Pure Rust, in synth-core** (`synth_core::swiftf0`) — streaming inference,
   379 KB of weights compiled in, ~15× realtime on an M1 (≈5 % of a core),
   already selectable in `superduper-tune` via its `Model` param. Because it
   lives in synth-core it **already cross-compiles to iOS** through
   `mobile/sdsp-ios` — nothing links it yet.
2. **Core ML package** (`~/Music/1music/swiftf0-coreml/`, 212 KB) — the same
   CNN converted for the Apple Neural Engine, measured at **1.05 ms per second
   of audio (~960× realtime)** on the M1 ANE versus 8.3 ms for ONNX CPU. Needs
   the STFT done outside the model (the recipe and the exact preprocessing
   contract are in that folder's README).

The live2play gesture instrument needs pitch: it drives the in-app synth from
the voice, and its current path has no neural option. The question this track
settles by measurement, not by argument: on an actual iPhone, is the Rust
inference cheap enough (it costs battery and shares the audio thread's
budget), or is the ANE route worth the extra plumbing (Core ML runs off the
audio thread, so results arrive a block late)?

## Acceptance Criteria

- [ ] `mobile/sdsp-ios` exposes a pitch-tracking C ABI (push samples → get
      Hz + confidence) backed by `synth_core::swiftf0`
- [ ] Measured on a real iPhone (not the simulator): CPU cost per second of
      audio, and the effect on the existing audio-thread headroom
- [ ] The same measurement for the Core ML path, including its off-thread
      latency, so the two are compared on one page with real numbers
- [ ] A documented recommendation with the numbers behind it, written into
      `mobile/sdsp-ios`'s docs and the reelcam side
- [ ] The chosen path is wired into live2play's voice→instrument mapping and
      audibly tracks a sung note
- [ ] Thermals: sustained 10-minute run does not push the app into the
      throttling behaviour recorded in `reference_reelcam_perf_pipeline`
- [ ] Fallback: if the neural tracker is unavailable or too costly at runtime,
      the existing YIN path still works — the choice is a setting, not a fork

## Dependencies

- `synth_core::swiftf0` (shipped), `synth_core::pitch` (YIN, the fallback)
- `mobile/sdsp-ios` staticlib + `make ios` → XCFramework into reelcam
- `~/Music/1music/swiftf0-coreml/` — mlpackage + conversion README (needed only
  if the ANE path wins)
- reelcam / live2play app repo (`~/startups/active/reelcam`) for the
  integration half
- Attribution: SwiftF0 is CC-BY-4.0 — the app must credit it

## Out of Scope

- Retraining or fine-tuning the model
- Polyphonic pitch (SwiftF0 is monophonic by construction)
- Replacing YIN in the desktop plugins (already done as a `Model` param)
- Shipping the Core ML package as a download-on-demand asset unless the
  measurement says the ANE path wins

## Technical Notes

- **The Rust path is nearly free to try**: synth-core is already an iOS
  dependency of `mobile/sdsp-ios` with `default-features = false`, so the
  tracker compiles today. Start there and measure; only build the Core ML
  plumbing if the numbers demand it.
- The weights are a 379 KB `include_bytes!` blob — that lands in the app
  binary. The Core ML package is 212 KB but is a directory of files and wants
  download-on-demand handling (see the `model-shrink` skill's delivery
  section).
- Core ML's ANE result is impressive but arrives asynchronously: the audio
  thread cannot block on `MLModel.predict`. Practically that means one block
  of extra latency and a shared buffer — acceptable for gesture mapping,
  probably not for anything sample-locked.
- Apple-platform measurement rules from the `model-shrink` skill apply: measure
  on the device, never the simulator; interleave variants within each round;
  the ANE's first load cost 43 s of one-time compilation in one recorded case
  (0.2 s in ours — verify on the phone).
- The debug HTTP endpoint pattern already used in reelcam (`:9010 /gesture`,
  `/snapshot`) is the cheapest way to run these measurements from the Mac —
  add a `/pitch` endpoint that reports the tracker's cost and current estimate.
- SwiftF0's own confidence output is the voiced/unvoiced gate (≥0.5); YIN has
  no equivalent and holds its last value instead, so the mapping layer must
  handle both shapes.

## Related work

- `~/Music/1music/swiftf0-coreml/README.md` — conversion recipe, preprocessing
  contract, M1 benchmark table
- `tools/pitch-bench` — the desktop A/B harness; its accuracy table is the
  reason to want SwiftF0 on the phone (YIN loses the octave on noisy input in
  100 % of frames, SwiftF0 stays within 1.3 cents)
- Memory notes: `reference_swiftf0_coreml`,
  `reference_live2play_inapp_synth_chain`, `reference_reelcam_perf_pipeline`
