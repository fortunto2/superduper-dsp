# Specification: PSOLA scope guard — pick the pitch engine by material

**Track ID:** psola-engine-choice_20260827
**Type:** Bug
**Created:** 2026-08-27
**Status:** Shipped 2026-08-27

## Summary

`synth_core::psola` (TD-PSOLA) is shared by **superduper-pitch** (Voice mode)
and **superduper-tune** (autotune). Measured at unity shift, it is transparent
on material with real glottal epochs and destroys material without them:

| source at unity shift | noise-to-harmonic in | out |
|---|---|---|
| impulse-train voice (pulses → formant resonators) | −23.9 dB | −24.2 dB |
| smooth harmonic tone (synth / sustained kubyz / sum-of-sines) | −66.9 dB | **−2.6 dB** |

Cause: a grain is READ around a snapped energy peak (`refine_epoch`, ±T0/2)
but WRITTEN to an unsnapped uniform synthesis mark. On a pulsed signal the
peak is the glottal pulse and the offset is small and consistent; on a smooth
signal there is no pulse, the "peak" wanders across the period, and pulses
overlap-add at scrambled phase.

This is a **scope** defect, not an algorithm defect. Three attempted fixes were
measured and reverted (lesson 24 in CLAUDE.md): snapping both read and write
points removes the pitch shift entirely (+12 st measured as −16 st);
sequencing epochs from the previous one made the noise worse (−5 dB); lowering
the OLA window-sum floor changed nothing. So the work is to **detect whether
the material suits PSOLA and route to the phase vocoder when it does not** —
`pvoc` (in superduper-pitch) measures −66.9 dB on the same smooth tone.

A second, independent finding to fold in: PSOLA's pitch floor was hardcoded at
95 Hz, so a bass or low male voice (F2 = 87 Hz) never locked. `with_range()`
now exists but no plugin passes anything but the default.

## Acceptance Criteria

- [x] A measured "epoch sharpness" signal descriptor exists in `synth-core`,
      RT-safe, that separates the pulsed voice from the smooth tone on the two
      test signals already in `engine_transparency.rs`
- [x] With auto engine selection, unity-shift noise-to-harmonic stays within
      2 dB of the input on BOTH test sources — measured −24.2 dB pulsed and
      −66.8 dB smooth against −23.9 / −66.9 inputs (smooth was −2.6 dB)
- [x] `superduper-pitch` Mode gains an `Auto` setting (existing Voice/Track
      values keep their indices and meaning — REAPER caches param layouts)
- [x] `superduper-tune` corrects a smooth synthetic tone to within 10 cents
      without adding audible noise — `tests/closed_loop.rs` measures 0.0 cents
      at a −47.4 dB noise floor (forced PSOLA: −2.7 dB). The bound there is
      absolute, not "no rise": the input is an exact sum of sines at −66.9 dB
      and no shifter holds that once it actually shifts
- [x] Engine switches never click: `max |x[n+1] − x[n]|` stays under the
      existing `click_audit` bound across a switch, including mid-note —
      measured 0.0095 during the switch against 0.0095 steady, and the level
      holds too (the crossfade law was picked from the r = 0.13 correlation
      between the engines, not by taste)
- [x] Low voices work: a 87 Hz take is tracked and corrected (needs the
      plugins to pass a real floor to `with_range`, or to derive it) — the
      floor is 70 Hz, fixed at construction because 4·T0_max look-behind means
      an adaptive floor would be adaptive latency. Costs 42 → 57 ms
- [x] All existing tests green (now 27 / 10 / 93):
      synth-core 82
- [x] `process()` still allocates nothing (sdsp-test-kit's counting allocator)

## Dependencies

- `synth_core::psola` (`with_range` already landed), `synth_core::pitch`
- `superduper_pitch::pvoc` — the fallback engine, already measured
- `tools/pitch-bench` — the CPU/accuracy harness pattern to copy for A/B
- No external crates expected

## Out of Scope

- Rewriting the PSOLA grain scheduler (three attempts failed; the scope guard
  makes it unnecessary for shipping quality)
- Laroche-Dolson phase-locking / cepstral formant preservation for `pvoc`
  (deferred item 5, unrelated)
- `tools/sdsp-tune`, which already uses `pvoc` deliberately

## Technical Notes

- The two reference signals already exist in
  `effects/superduper-pitch/tests/engine_transparency.rs` (`tone()` = smooth,
  the pulsed voice inside
  `psola_transparency_depends_on_epoch_sharpness`). Reuse them; do not invent
  new ones, so numbers stay comparable to the recorded baseline.
- Candidate descriptors for epoch sharpness, cheapest first: peak-to-RMS
  within one tracked period; ratio of the strongest sample to the period mean;
  spectral flatness. The first is a few ops per period and needs no FFT — try
  it before anything spectral.
- `pvoc` reports ~43 ms latency and PSOLA ~21–59 ms depending on the floor.
  Both plugins report latency to the host for PDC, and REAPER does not like it
  changing at runtime — so the reported latency must be the **max** of the two
  engines, fixed at activate, exactly as `with_latency`'s `min_latency`
  argument was designed for (see `PitchShifter::with_latency` doc).
- Switching engines mid-stream needs a short crossfade (both engines run for a
  few ms) or the switch must be gated to note boundaries / silence. The
  crossfade is simpler and matches how `superduper-wind` handles its
  deferred-steal fades.
- `superduper-tune` already carries the `Model` param precedent (YIN vs
  SwiftF0, added 2026-08-27) for exposing an engine choice as a stepped param
  with `value_to_text` naming — copy that shape.
- Param-table changes are frozen-by-convention (lesson 10): only APPEND, never
  reorder, and re-record `tests/quality.snap` deliberately with
  `SDSP_UPDATE_SNAPSHOTS=1`.
