---
name: audio-to-midi
description: Turn a recording into MIDI — pick the right transcription model, run it locally, and check the result before trusting it. Use when the user says "сними партию", "вытащи миди из трека", "audio to MIDI", "какие там ноты", "переиграй это нашими плагинами", "сделай ноты из записи", or drops audio expecting notes rather than a mix. Covers MuScriptor (full-mix multi-instrument), Basic Pitch (fast, licensable, velocity), measured speed and accuracy on Apple Silicon, what each model gets wrong, and how the MIDI lands in the SuperDuper instruments. Do NOT use for stem separation (stem-split skill) or for analysing a reference's mix (track-teardown).
---

# audio-to-midi — from a recording to notes you can re-play

Two models cover everything we need, and they fail in opposite directions. Pick
by what you are after, not by which is "better".

| | **MuScriptor** medium | **Basic Pitch** |
|---|---|---|
| input | full stereo mix, no stems needed | any audio |
| output | one labelled MIDI track **per instrument** + drums in GM | one untitled track, tonal only |
| drums | yes, GM map (36 kick / 38 snare / 39 clap / 42 hat) | **none** — percussion ignored |
| tempo / key | detected, accurate | **placeholder 120 BPM** |
| velocity | **all notes 100**, no dynamics at all | real, 30 distinct values (36–72) |
| pitch bend | no | yes |
| licence | code MIT, **weights CC BY-NC 4.0** | **Apache-2.0**, ours to ship |
| speed (40 s audio) | 24 s on Metal | **11 s on CPU** |
| what it is | Kyutai + Mirelo, July 2026, decoder-only transformer | Spotify, small CNN, Core ML on macOS |

So: **MuScriptor to understand a track, Basic Pitch to put transcription in a
product.** The non-commercial licence on the weights rules MuScriptor out of
anything that ships — fine for our own teardowns, not for reelcam or livehub.

## The commands

```bash
# MuScriptor — needs a HuggingFace token, weights are gated
export HF_TOKEN=hf_...
uvx muscriptor transcribe mix.wav --output out.mid --detect-tempo best-effort
uvx muscriptor transcribe mix.wav --format sheets --output score/   # +MusicXML, PDF, tabs
uvx muscriptor list-instruments                                     # names for --instruments
uvx muscriptor serve                                                # local web UI

# Basic Pitch
mkdir -p bp_out
uvx --python 3.11 --with "setuptools<81" --from basic-pitch basic-pitch bp_out mix.wav
```

Four traps, each one cost a run:

- **`--output` wants a file, not a directory.** A trailing `out/` transcribes
  fine and then dies with `IsADirectoryError` after the generation is done. The
  directory form belongs to `--format sheets` only.
- **The HF gate is per size.** Accepting the licence for `muscriptor-medium`
  leaves `small` and `large` at 403. Accept each on its own model page.
- **Basic Pitch needs `setuptools<81`.** `resampy` imports `pkg_resources`,
  which setuptools 81 removed, so a clean environment fails with
  `ModuleNotFoundError: pkg_resources`. Plain `--with setuptools` does not fix
  it — the pin is the fix.
- **Basic Pitch will not create its output directory.** `mkdir -p` first, or
  `🚨 bp_out is not a directory`.

## Model sizes and where the compute goes

*Measured here, M5 / 32 GB, medium, the same 40 s excerpt.* No conversion to
Core ML or ONNX is needed on Apple Silicon: the package checks
`torch.backends.mps.is_available() and platform.machine() == "arm64"`
(`accelerator.py:25`) and moves the plain PyTorch weights to Metal itself.

| configuration | generate | × realtime | a 4:49 track |
|---|---|---|---|
| MPS fp16 (default) | 24.1 s | 0.60× | ~2.9 min |
| CPU fp16 | 66.5 s | 1.66× | ~8 min |
| CPU fp32 | 63.1 s | 1.58× | ~7.6 min |
| MPS fp16 `-b 8 --no-prelude-forcing` | 6.6 s | 0.16× | ~48 s |

- **A GPU is not required.** CPU is 2.8× slower, not orders of magnitude. On
  CPU, fp32 is marginally *faster* than fp16 — half precision is emulated there.
- MPS fp16 and CPU return near-identical MIDI (206 notes against 207 on the
  lead), so half precision on Metal costs nothing.
- **`--batch-size 8` buys 3.7× and eats the parts.** Drums survive intact (kick
  52, snare 22, clap 4, identical), hats lose 7%, and the lead collapses from
  **206 notes to 84** — without prelude-forcing a chunk cannot see the previous
  one. Use it to grab a grid and a rhythm section; never to lift a part.
- Sizes: `small` 103M (CPU), `medium` 307M (default), `large` 1.4B (their docs
  ask for a GPU; untested here — the gate was not accepted for it).
- `--dtype`, `--device`, `--model`, `--instruments` all exist; `--instruments`
  *forbids* everything not listed, which is the cheap way to stop a synth being
  decoded as three overlapping guitars.

## What the numbers are worth

*Measured here* against a by-hand teardown of Gesaffelstein "Hate or Glory"
(40 s of the main section; the hand analysis is `demos8/ANALYSIS.md`):

| | by hand | MuScriptor |
|---|---|---|
| tempo | 102.985 BPM | **102.994** (librosa said 103.36 and drifted a beat by bar 88) |
| key | G minor | G / D / A# — G minor |
| bass | G1, 48.6 Hz | **G1**, 158 notes |
| kick | four-on-the-floor | 51 consecutive beats, **0 missing**, median offset −1.2 ms, max 19.4 ms |

That grid accuracy is the part worth having: a transcription with the right
notes on a wrong grid cannot be rebuilt from. Their published onset F1 is 60.4,
so expect roughly this on rhythmic material and worse on anything loose.

**Three things not to believe**, all three measured:

1. **Velocity.** Every note comes out at 100 — all 657 of them. The model has no
   velocity representation, so dynamics must be written afterwards or the part
   plays like a machine. Basic Pitch does give real velocities.
2. **Durations.** The measured bass is a 540 ms stab (0.93 of a beat); the model
   returned a median of 0.26 and shredded the pedal note into re-attacks. It
   holds no sustain. Onsets and grid, yes; length and articulation, measure from
   the signal.
3. **Instrument names.** A synth lead came back as `prog=29 distorted electric
   guitar`. The part is right, the GM label is a guess — reassign it yourself.

Also from their docs: rubato recordings notate significantly worse. Beat-driven
references transcribe well, ambient is not worth the run.

## Into our instruments

The drum track already uses the GM map, which is what **SuperDuper Drum** reads
with `Note Map` on `0 Auto` (36/35 kick, 38/40 snare, 39 clap, 42/44 hat) — the
MIDI drops in with no remapping. Tonal tracks go to Wave or Kubyz, with the GM
program ignored.

Build the arrangement by writing the `.rpp` directly rather than pushing items
through the REAPER bridge — see the project CLAUDE.md, and `demos5/build_rpp.py`
for a working generator. For notes there is one more route: the bridge's generic
passthrough via `reaper_ipc.py`, which inserts notes in bulk without inlining
thousands of them into tool arguments.

## Check before you trust

```bash
uvx --with mido python scripts/midinspect.py out.mid
uvx --with mido python scripts/gridcheck.py out.mid drums 36
```

`midinspect` prints per-track note counts, ranges, pitch-class histograms and
duration medians — enough to see the key, spot a doubled octave and catch a part
that came back empty. `gridcheck` answers the question a note list cannot:
whether the onsets sit on the beat grid, with the deviation in ms and every
beat that was missed. An empty result exits 2 and says nothing was measured,
because "no findings" and "nothing checked" must not look the same.

## Not measured here

Reported but unverified in this repo, in case a run needs more than the two
above: **NeuralNote** (Apache-2.0, VST3/AU wrapper around Basic Pitch, actively
developed — 2026-09-16 — so the way to get this inside REAPER without a CLI),
and **MT3** (Google, multi-instrument, JAX/T5X, awkward to run). Mirelo's hosted
Audio-to-MIDI Pro is the same lineage as MuScriptor with a more accurate closed
model and an API, at <https://mirelo.ai/models/audio-to-midi>.
