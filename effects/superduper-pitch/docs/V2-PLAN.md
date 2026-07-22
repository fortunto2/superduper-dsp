# SuperDuper Pitch — v2 work plan

Feature + fix batch requested after live testing. Research-backed approaches
below. Every item must ship with a test.

## Bugs to fix (priority order)

### 1. Voice mode clicks (USER-REPORTED, highest priority)
Symptom: audible clicks in Voice (PSOLA) mode. Cause: the 1-period-grain OLA
has grain-boundary discontinuities (grains not epoch-aligned / insufficient
overlap / no crossfade).
Fix approach:
- Place grains **pitch-synchronously on detected epochs** (glottal closure
  instants) rather than at fixed T0 offsets, so grain edges land at low-energy
  points. Even without true epoch detection, use ≥2·T0 Hann grains with ≥50 %
  overlap and normalize by the summed window, and **crossfade** overlapping
  grains (equal-power) instead of hard concatenation.
- On pitch-down, the gaps between 1-period grains are the click source →
  overlap the source grains (read 2·T0, hop by synthesis mark) and rely on the
  window sum for unity gain, OR duplicate/crossfade the last grain to fill gaps.
- Add a click-audit test (pattern from `superduper-pad/tests/click_audit.rs`):
  drive a sustained voice-like tone through Voice mode at −5 / 0 / +5 st, assert
  `max |x[n+1] − x[n]| < 0.3` and no sample spikes.

### 2. Track mode peak overshoot (measured 2.67× at identity)
Symptom: even at Pitch 0, Track output peaks ~2.67× the input → clips on drums.
RMS is correct (unity), so it is an OLA **peak/window normalization** issue plus
transient overshoot.
Fix approach:
- Correct the analysis+synthesis window normalization so **Pitch 0 is ≈ identity
  in peak** (Hann² at 75 % overlap sums to 1.5 — divide it out).
- Add **transient detection + phase reset** (spectral flux onset → reset phases
  that frame) — removes phasiness AND cuts transient peak overshoot on drums.
- Optional safety: soft-clip / small headroom on Track output.
- Test: transparent Track (Pitch 0) on a transient signal → `out_peak ≤ 1.1×
  in_peak` (currently 2.67×). Keep `polyphony.rs` chord test green.

## Algorithm upgrades (research-backed — make it "as good as possible")

Sources: Laroche & Dolson phase-locking; smbPitchShift; élastique (zplane) is the
commercial quality baseline; Röbel transient handling; formant preservation via
spectral envelope (whitening + envelope reconstruction).

- **Phase-locking (identity / rigid)** in the Track phase vocoder — lock the
  phases of bins around each spectral peak to the peak's phase. This is the
  single biggest quality win against "phasiness"/"reverberant" artifacts on
  polyphonic material. (Laroche-Dolson 1999.)
- **Formant preservation** for Track (currently only Voice/PSOLA has it): estimate
  the spectral envelope (cepstral liftering or LPC), shift the fine structure by
  α, keep/scale the envelope separately by 2^(Formant/12). Lets you transpose a
  vocal-in-a-mix without chipmunking it.
- Keep Voice (PSOLA) for monophonic voice (best formant control), Track (phase
  vocoder) for polyphony — the dual-engine split is correct.

## New features

### 3. Show the detected KEY of the incoming audio (Krumhansl-Schmuckler)
- Build a **chromagram**: 12 pitch-class energies accumulated over a rolling
  window (map FFT bin magnitudes → pitch classes). Reuse the Track STFT frames
  so it is nearly free.
- Correlate the 12-vector against the **24 Krumhansl-Schmuckler key profiles**
  (12 major + 12 minor, transposed to each tonic) via cosine/Pearson correlation
  → best match = detected key (e.g. "A minor", "C major").
- **Display it in the GUI** (a live readout, e.g. under the scope: `Key: A minor
  (conf 0.82)`). Update ~2-4×/sec off the GUI thread reading a shared chromagram
  snapshot (lock-free, like LiveScope).
- Test: feed a C-major scale/chord progression → detects C major; A-minor → A
  minor; a transposed copy → the transposed key.

### 4. Quick key MATCH between tracks
Workflow (in-plugin, since the plugin only sees its own track):
- Each instance **displays its detected key**. Put the plugin on the reference
  track (e.g. the vocal) → read its key. Put it on the track to move (e.g. the
  music).
- Add a **`Target Key` selector** (None / C / C# / … + Maj/Min) in the GUI. When
  set, the plugin computes the semitone interval from *detected key* → *target
  key* and shows the suggested shift, with a **"Match" button** that sets `Pitch`
  to that interval (nearest-octave, so it does not jump wildly). Track mode.
- Result: read the vocal's key on its instance, type it as the Target Key on the
  music's instance, hit Match → the music is transposed into the vocal's key.
- Keep it simple: Target Key is a stepped param or GUI-only state (persisted via
  state ext). The Match action writes `Pitch` (dirty + gesture so it records).
- Test: detected key C major + target A major → suggested/applied shift = −3 (or
  +9 nearest octave); applying it and re-detecting yields A major.

## Verify everything with tests
`cargo test --release -p superduper-pitch` must stay green with all new tests:
click-audit (Voice), transparent-peak (Track), phase-lock quality (optional
metric), key-detection accuracy, key-match interval. Plus rebuild the bundle and
re-run the external audit harness in the scratchpad (`pitchaudit`) to confirm
peaks ≤ ~1.1× and no NaN.

## Notes
- Latency: key detection + phase-locking must not add latency beyond the STFT
  window already reported. Formant/transient work stays inside the existing
  frame budget.
- RT-safe: no heap in `process()`; chromagram accumulation into a pre-allocated
  `[f32; 12]`, GUI reads a snapshot.
