---
name: track-teardown
description: "Reverse-engineer a reference track (tempo, key, groove grid, band balance, arrangement dynamics) and rebuild its vibe with the SuperDuper plugins. Use when the user points at a song and says \"make it in this style\", \"break this track down\", \"take this vibe\", \"make it like X\", \"what is going on in this track\", or drops an audio file expecting analysis. Covers how to see a track (spectrograms as images, demucs stems) and how to converge a rebuild onto the reference by measurement. Do NOT use for mixing an existing render (sdsp-mix) or writing an arrangement from scratch (superduper-song)."
---

# track-teardown — hear a reference by looking at it, then rebuild it

An agent cannot listen. It can *see*, and a spectrogram is a picture of sound:
the kick pattern, the sidechain pumping, where the bass enters, how long the
tails are, where the sections cut. Read the PNGs. They are the ears.

Three levels, in order:

1. **Pictures** — mel spectrograms of the whole track and of 4-bar close-ups.
2. **Stems** — `demucs` splits drums/bass/other/vocals, so each layer can be
   looked at alone. This is the solo button.
3. **Numbers** — tempo fitted to the hits, band balance vs the reference,
   per-4-bar energy. Only after the pictures, or the numbers mislead.

> **Needs** Python 3.11+ with `numpy`, `scipy`, `librosa`, `matplotlib`, `soundfile`, and `demucs` (via `uvx`) for stems; the analysis
> runs on local files only (demucs downloads its model weights on first use). Rebuilding with plugins additionally needs the
> `sdsp-chain` tool (see the `sdsp-chain` skill). Check first: `python3 -c "import numpy, scipy, librosa, matplotlib"`; if it fails,
> offer the `uv venv` + `uv pip install` lines below instead of failing mid-run.

## Setup

```bash
uv venv .venv
uv pip install --python .venv/bin/python numpy scipy librosa matplotlib soundfile
uvx --with numpy --from demucs demucs -n htdemucs -o stems "<track>"   # ~3 min
```

## The pipeline

Scripts live in `${CLAUDE_SKILL_DIR}/scripts/` and write their PNGs to `./ref/` in the current directory.

```bash
.venv/bin/python ${CLAUDE_SKILL_DIR}/scripts/analyze.py "<track>"               # tempo, key, spectrogram, bands -> ./ref
.venv/bin/python ${CLAUDE_SKILL_DIR}/scripts/zoom.py "<file>" name 74.5 4 103   # 4-bar close-up + waveform (start s, bars, bpm)
python3 ${CLAUDE_SKILL_DIR}/../sdsp-mix/scripts/mixcheck.py ref.wav             # band balance, crest, correlation
```

Tempo fitting, per-4-bar arrangement energy, the kick/bass groove grid and the gain solver were
written for one specific track and are kept as worked examples in the repository, not in this plugin:
`docs/examples/track-teardown/` (`tempo.py`, `sections.py`, `kickmap.py`, `balance.py`). They need their
constants (`MIX`, `BPM`, `T0`) edited for a new track, and `balance.py` and `sections.py` import helper modules (`build`,
`groove`) that belong to that rebuild and are not included. Where the text below names them, treat them as
recipes to re-create, not commands to run.

Then **look at every PNG**. `ref/*_full.png` shows structure, `ref/zoom_*.png`
shows groove. A dense vertical picket fence across the whole track means
everything is gated in rhythm. A dark top with two bright horizontal bands
means a scooped, two-hump spectrum. You can see both before measuring either.

## Five traps that cost hours

**Folding the whole track into one bar tells you nothing.** A 289 s song passes
through several grooves, so every 1/16 slot fills and the histogram reports
"all sixteen". Window it to one section (`sections.py` does).

**Onset detection on a demucs stem fires on leakage.** The drum stem carries
distortion tails from the synths. Below 90 Hz only the kick and the bass
fundamental exist, so a lowpassed Hilbert envelope gives an unambiguous grid
(`kickmap.py`). This changed the read from "hits on every 1/16" to "kick on
every beat".

**A BPM good to 0.4 is not good enough.** At 103 BPM that drifts a full beat by
bar 88 — the kick then appears on a different 1/16 in each section and looks
like a groove change. Fit tempo *and* phase over the whole track (`tempo.py`);
librosa's 103.36 was really 102.985.

**The key is not always where the pitch tracker says.** Check the bass
fundamental directly: one pedal note restruck every beat is common, and its
frequency (48.6 Hz = G1) is more reliable than a chroma estimate.

**Energy is usually made of brightness, not volume.** Measure the high band
per 4 bars before assuming the arrangement is about levels. In the reference
here, total level moved 21 dB across the track and the 3 kHz+ band moved
**46 dB** — three plateaus, each brighter than the last, with the sub yanked
out entirely twice as an effect. Copy *that*, not the fader moves.

## Rebuilding: solve the balance, do not tweak it

Band balance is **relative** — every band is measured against the loudest one.
So raising the bass to fill 120-250 Hz pushes all six other bands down, the
next edit fixes one and breaks two more. Four hand passes went
−11.7 → −13.8 → −10.1 → −9.8 dB in the band they were aimed at. It does not
converge.

`balance.py` solves it instead: render each track solo, measure its energy per
band, fit the gains whose sum lands closest to the reference profile.

```bash
.venv/bin/python balance.py --dry          # solve and print
.venv/bin/python balance.py --iterate 4    # close the loop around the master bus
```

Getting that solver right took four separate corrections, all worth knowing:

| Symptom | Cause | Fix |
|---|---|---|
| Solver returns −54000 dB for four layers | unconstrained: muting layers fits the curve | bounds, and per-layer floors for musical role |
| Perfect prediction, render 12 dB off | solo rendered *with its fader applied*, then solved for an absolute gain — counted twice | solo renders at unity |
| Prediction right, mixcheck disagrees | solver averaged energy per bin, mixcheck sums it — narrow bands got equal weight to wide ones | use the same metric as the report |
| Loop diverges 10.7 → 14.0 dB | full-step compensation on a non-linear master bus | damp to ~0.45, clamp, and **keep the best measured pass, not the last** |

And the finding no fader can fix: if the per-layer table shows **no layer peaks
in the band you are short in**, that is a missing sound source, not a level. In
this build nothing peaked at 120-250 Hz — the reference's loudest band — until
a mid-bass voice was added with its fundamental at 138 Hz by construction.

## Where the vibe actually lives

For an industrial/techno reference, the parts that carry it:

- **kick** on every beat, short, with a pitch drop;
- **one bass note** restruck every beat, distorted into a harmonic ladder,
  with the upper harmonics decaying faster than the fundamental (a filter
  envelope on top of the amplitude one);
- **a mid layer gated in *uneven* 1/8s** — even gating pulses, uneven grinds;
- **noise on top**, not hi-hats, opened up section by section;
- **near-mono** (correlation +0.88, side/mid −12 dB) — width is not the point.

Any of these can be carved out of a single sample. A jaw harp pluck is a
transient *and* a drone, so kick (−24 st + lowpass + saturator), bass (−12 st +
saturator + filter envelope), mid-bass (+12 st), grind (plucks overlapping at
1/8), and top (grains pitched +12) can all come from one file — which is what
makes the result yours rather than a copy.

## Gotchas in our own tools

- **`duck_from`/`duck_db` in sdsp-chain is not a depth in dB.** It fixes the
  threshold at −26 dB and turns `duck_db` into a ratio, so `3.5` against a kick
  peaking at 0 dBFS asks for ~20 dB of gain reduction — held permanently when
  kick and bass share every beat. Use an explicit `[[track.stage]]` compressor
  with `sidechain = "track:kick"` and real numbers.
- **An automated LP that dips below a fixed HP silences the layer.** It read as
  −36 LUFS and no gain would have fixed it.
- **A stretched drone has almost no energy above 800 Hz.** PaulStretch keeps
  the low formants and smears the rest; it cannot supply a mid or top layer no
  matter how hard it is driven. Use plucks for those.
- **Always end with `mixcheck.py --ref`.** Wire it into the build script so
  every render prints the comparison; levels set by ear in a text editor were
  24 dB out in the mid band on the first pass.

## Worked example

One reference track was torn down and rebuilt entirely from a single kubyz pluck with a build script and
the gain solver, converged to within 1 dB on six of seven bands. A second script put the same mix into REAPER for hand
work, reading the settings from the very TOML the render used, so there is one source of truth. The scripts are not part of this plugin.

## Снятие партий в MIDI

Ноты, сетку и деление на инструменты снимает MuScriptor по полному миксу, стемы
не нужны, и барабаны приходят в GM-карте прямо для нашего Drum. На этом же
Gesaffelstein: темп 102.994 против измеренного 102.985, бас G1, кик 51 доля
подряд без пропусков. Чему нельзя верить — velocity (все ноты 100), длительности
(педальную ноту дробит на атаки) и названиям инструментов.

Команды, размеры моделей, скорость на Apple Silicon и проверка результата —
скилл **audio-to-midi**. 
