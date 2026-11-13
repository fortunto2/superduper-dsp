---
name: audio-to-midi
description: "Turn a recording into MIDI: pick the right local transcription model, run it, and check the result before trusting it. Use when the user says \"audio to MIDI\", \"get the notes out of this track\", \"transcribe this melody\", \"what notes are being played\", \"which strings and frets\", \"replay this with the SuperDuper instruments\", or drops audio expecting notes rather than a mix. Covers MuScriptor (full-mix, multi-instrument), Basic Pitch (fast, velocity), SwiftF0 (monophonic line) and guitar-hear (string and fret on top of Basic Pitch), with measured speed and accuracy on Apple Silicon, what each gets wrong, a known-answer control that catches a silently broken run, and how the MIDI lands in the SuperDuper instruments. Do NOT use for stem separation or for analysing a reference's mix (track-teardown)."
---

# audio-to-midi — from a recording to notes you can re-play

> **Needs** `uv` (for `uvx`) and an internet connection the first time, because the models are downloaded then (MuScriptor and Basic Pitch
> weights from their upstream hosts; MuScriptor's weights are gated on Hugging Face and need the user's own token, which stays in their
> shell). The two checking scripts in `${CLAUDE_SKILL_DIR}/scripts/` ship with the plugin and need `mido`. **Not bundled:** the SwiftF0
> tracker (`pitch_line.py`, `midi_theme.py`) and `guitar-hear` are the author's separate tools, not part of this plugin;
> the sections about them describe results, and when asked to run them say they are not installed and fall back to Basic Pitch or MuScriptor.
> Check first: `command -v uvx`.

Three models, all local, failing in different directions. Pick by **what the
material is**, not by which is "better".

| | **MuScriptor** medium | **Basic Pitch** | **SwiftF0** |
|---|---|---|---|
| what it hears | full mix, several instruments | polyphony, one track | **one line** — the loudest |
| output | labelled MIDI track per instrument + GM drums | one untitled track, tonal only | pitch + confidence per hop |
| drums | yes, GM map (36 kick / 38 snare / 39 clap / 42 hat) | **none** | none |
| tempo / key | detected, accurate | **placeholder 120 BPM** | none — it is a tracker |
| velocity | **all notes 100** | real, 30 values (36–72) | none |
| licence | code MIT, **weights CC BY-NC 4.0** | **Apache-2.0**, ours to ship | CC-BY-4.0 (lars76), **converted by us** |
| speed | 52 s for 3:12 of audio | 35 s for the same | **1 s for the same** (~190x RT) |
| where | `uvx muscriptor`, weights cached | `uvx basic-pitch` | author's Core ML build, not bundled |

**Pick by material:**

- **a band, a track, drums** → MuScriptor. It is the only one that separates
  instruments and gets the grid right.
- **something that ships** → Basic Pitch. The NC weights bar MuScriptor from a
  product, and only from a product: transcribing your own recording for your own
  non-commercial use is the case the licence allows. (The bar was once read wider
  than it is written, and the better model was skipped for no reason.)
- **one melody — a voice, a flute, a solo line** → SwiftF0. It does not guess a
  chord, it follows a line, and it is two orders of magnitude faster than the
  other two.
- **a solo guitar, and the question is the hand** (which string, which fret,
  how long the picking circle is) → the guitar decoder described below
  (results and method only; the tool is not bundled). The other three give
  notes; none of them gives the string.

## Guitar: which string, which fret (guitar-hear, not bundled)

A decoder on top of Basic Pitch's own posteriors (the same `nmp.onnx`,
Apache-2.0): six strings, one note per string,
every note placed where the tuning and capo allow, the cheapest hand path,
re-plucks of a still-ringing string read from the onset head.

**guitar-hear is the author's separate tool and is not part of this plugin;** it has no public repository. This section
gives its measured results and the method, so the approach can be rebuilt. If asked to run it, say it is not installed and
offer Basic Pitch or MuScriptor for the notes. Its commands, for reference (`<guitar-hear checkout>` is wherever the tool lives):

```bash
cd <guitar-hear checkout>
./run.sh gh.py dec take.wav --capo 1 --maxfret 5   # cache/take.gh.json + take.gh.mid (MIDI channel = string)
./run.sh bench.py --set SS_comp --players 04,05    # GuitarSet check on held-out players
```

Measured on GuitarSet (mono mic, onset F1 at 50 ms, tuned on players 00-03,
scored on 04-05):

| set | Basic Pitch F1 | guitar-hear F1 | re-pluck recall BP / ours | string right |
|---|---|---|---|---|
| singer-songwriter comping | 0.624 | 0.630 | 0.824 / 0.850 | 79 % |
| bossa nova comping | 0.761 | 0.775 | 0.775 / 0.816 | 55 % |

Same notes as Basic Pitch, plus strings and frets. On a real solo take
(acoustic, capo 1, 66 s) it found the whole pattern: thumb on the 6th string
(frets 3/2/0), fingers `[bass + Ab3 + C4] Eb3 Ab3 Eb3` per beat, a three-bar
circle. Replayed against the recording (log-CQT cosine / onset correlation):
guitar-hear 0.760 / 0.497, Basic Pitch 0.751 / 0.492, MuScriptor 0.655 / −0.126.

- **Read Basic Pitch's frames with its windowing.** It runs 2 s windows of 172
  frames and each starts 10.24 ms early, so frame `f` is at
  `f*256/22050 − 0.01024*(f//172)`. The naive `f*256/22050` drifts 0.3 s a
  minute and made a good decode look like noise (precision 0.14 → 0.30 from
  this fix alone).
- **Frets above the 5th are guesses.** The hand model is naive: 55 % strings
  on bossa nova, where the chords move up the neck. Fine for open-position
  picking, check anything higher by ear.

**Rust, for a plugin:** `tract-onnx` 0.21 runs `nmp.onnx` with the same output
as onnxruntime (max difference 8·10⁻⁷ on all three heads), 80 ms per 2 s window
on an M1, 93 ms to load. Pure Rust, so no onnxruntime dylib inside a host.
The probe lives in the same unpublished tool; the numbers above are what to expect.

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

# SwiftF0: not bundled (the author's own tools); shown for the contract below
python3 scripts/pitch_line.py solo.wav --notes            # notes with timing
python3 scripts/pitch_line.py solo.wav --theme            # a --chords string
python3 scripts/midi_theme.py out.mid --windows 8         # the same from any MIDI
```

`pitch_line.py` carries SwiftF0's **preprocessing contract**: the ONNX graph contains an STFT node that no converter
accepts, so the model is cut open and the STFT is computed outside: 16 kHz mono, 384 zeros of padding on both sides
(constant, not reflect), frame 1024 / hop 256, asymmetric Hann, rFFT bins 3..135, `log(x + 1e-8)`. A deviation in any of
these **does not break the run**; it quietly spoils the pitch. The decoder is already inside the model: it returns `pitch_hz`
and `confidence`, so do not decode a softmax on top of them.

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
(40 s of the main section; the hand analysis is not part of this plugin):

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

## One recording, three models: what can be claimed

*Measured here, 23 Sep 2026.* A street piano recorded with a DJI mic in the middle of Navy Pier: wind, cars, voices. 3:12 of audio,
three models run independently:

| | notes | time | chords over eight windows |
|---|---|---|---|
| SwiftF0 | 325 (82% confident frames) | **1 s** | `Am Am Cm Dm Bm Dm Am E` |
| Basic Pitch | 547 | 35 s | `Am Am C Dm F Dm Bm Em` |
| MuScriptor | 376, track `acoustic piano` | 52 s | `Am Bm Dm Bm Am C F` |

**All three agree on Am.** The start and the fourth window match in two of three. Each has its own middle, so claim nothing about the middle.

The practical rule for a noisy recording: **harmony is recoverable, the motif is not.** A pitch-class histogram weighted by duration
averages out the false notes and gives a stable answer; the note sequence on such material is not reproducible (Basic Pitch's most
frequent upper-voice "figure" is one note three times in a row, and its range 36..95 includes octave jumps nobody played).
Inherit such a theme as a key and a chord progression, not as a melody.

## The control without which it lies quietly

**Run a 440 Hz sine first, before any material.** The answer is known in advance, and only that shows the instrument works.

Writing `pitch_line.py` produced two defects, neither visible in the output: decoding a softmax on top of values that were already
hertz (the model decodes internally), and closing a note only on a pitch change (a steady tone produced no note at all). Both times the
tool answered "no confident pitch found", and without the control the report would have been "the street piano is too noisy for a
monophonic tracker", a confident lie about the material.

A healthy answer looks like: `# 3.0s . notes 1 . confident frames 100%`, `0.00  A4  2.99s`, 437.5 Hz against 440, one logarithmic bin.

The same rule holds for any of the three models: **do not trust a zero from an instrument you have not seen return a non-zero.**

## Into our instruments

The drum track already uses the GM map, which is what **SuperDuper Drum** reads
with `Note Map` on `0 Auto` (36/35 kick, 38/40 snare, 39 clap, 42/44 hat) — the
MIDI drops in with no remapping. Tonal tracks go to Wave or Kubyz, with the GM
program ignored.

Build the arrangement by writing the `.rpp` directly rather than pushing items
through a REAPER bridge (see `CLAUDE.md` in the repo checkout, and the `superduper-song` skill). A bulk-insert route that
avoids inlining thousands of notes into tool arguments is better than many small calls.

## Check before you trust

```bash
uvx --with mido python ${CLAUDE_SKILL_DIR}/scripts/midinspect.py out.mid
uvx --with mido python ${CLAUDE_SKILL_DIR}/scripts/gridcheck.py out.mid drums 36
```

`midinspect` prints per-track note counts, ranges, pitch-class histograms and
duration medians — enough to see the key, spot a doubled octave and catch a part
that came back empty. `gridcheck` answers the question a note list cannot:
whether the onsets sit on the beat grid, with the deviation in ms and every
beat that was missed. An empty result exits 2 and says nothing was measured,
because "no findings" and "nothing checked" must not look the same.

## Not measured here

Reported but unverified in this repo, in case a run needs more than the three
above: **NeuralNote** (Apache-2.0, VST3/AU wrapper around Basic Pitch, actively
developed — 2026-09-16 — so the way to get this inside REAPER without a CLI),
and **MT3** (Google, multi-instrument, JAX/T5X, awkward to run). Mirelo's hosted
Audio-to-MIDI Pro is the same lineage as MuScriptor with a more accurate closed
model and an API, at <https://mirelo.ai/models/audio-to-midi>.
