# sdsp-score

A theme becomes an arranged piece played on SuperDuper DSP instruments,
headless: no REAPER, no film needed. Out come `score.mid` (a track per part),
the stems, the mix, a readout of how it sits, and a one-minute demo.

```bash
cd tools/sdsp-score && uv sync
uv run sdsp-score theme.json --out out/ --fit                                   # default song form
uv run sdsp-score theme.json --form "intro:8,verse:16,chorus:16,bridge:8,chorus:16,outro:8" --out out/
uv run sdsp-score theme.json --length 180 --out out/ --fit                      # form scaled to 3 min
uv run sdsp-score theme.json --length 90 --out out/ --midi-only                 # MIDI + toml, no audio
uv run sdsp-score theme.json,theme2.json film.form.json --out out/ --fit        # under a film
uv run sdsp-score check out/                                                    # read a render, change nothing
```

## Inputs

**theme.json** — `name`, `key`, `mode`, `bpm`, `chords`, and `motif`
(`[{deg, beats}]`) or `motif_midi` (a Basic Pitch MIDI; the most repeated
4-8 note figure becomes the motif; a relative path is relative to the
theme). `style` chooses the instruments and the flavour: `lead`,
`ostinato`, `arp`, `pad`, `bass` as `[instrument, preset]`, `kit`
(`kubyz` / `cassette` / `909`), `feel` (`double` hats or `straight`),
`swing`, `energy`, `under_motif`, `gains`, `form_tempo`
(`{"chorus": 1.05, …}`, factors of the theme bpm).

**form.json** — the only thing the core reads about time. Sections on a time
line (song kinds `intro/verse/chorus/bridge/outro`, film kinds
`card/talk/view`), cuts, speech windows, holes, accents, `key_audio`,
`theme2_for`, `tempo_map`, `bed`, `demo`. `--form` and `--length` generate
one; every run saves the form it used, so `out/form.json` reproduces it. A
film plan becomes a form in video-analyzer: `scripts/otio_to_form.py`.

## The song form

| section | textures (rotated per repeat) | drums |
|---|---|---|
| intro | halo / drone, a quiet thread of the motif underneath | none |
| verse | ostinato, arpeggio, motif sequence, retrograde | light |
| chorus | the motif, then inversion / sequence / augmentation; louder each repeat | full, crash in, tom fill before |
| bridge | inversion, retrograde — in the subdominant, entered through its dominant, left through home's | half time |
| outro | augmentation, halo, drone in 4-bar phrases, falling | soft kick |

Each section has its own tempo (default factors intro 0.94, verse 1.0,
chorus 1.05, bridge 0.92, outro 0.9; every tempo checked against 40..200).
Chords restart at every section, so a chorus is harmonically the same chorus.
No 8-bar window repeats the one before it: `diversity()` fingerprints every
window (part, bar, onset, pitch, duration) and the tests hold it at 0% for
three themes and three forms.

## Pipeline: arrange → mix → check → (fit)

- **arrange** (`arrange.py`, `music.py`) — pure: form + theme → notes.
- **mix** (`mix.py`) — stems at 0 dB, every level is a `gain_db` in the mix
  toml: one gain stage. The spec is saved as `mix.json`. Drum and 808 stems
  are written by python (float32, no peak normalisation) and enter the same
  mix. Not through the sdsp-chain sampler: its `Sample` is an index 0..255
  into a sorted bank, `SuperDuper Samples` holds 581 wavs (the kubyz kit is at
  345), and the render does not say which file played — a wrong index would
  sound silently.
- **check** (`check.py`) — reads only. One unit: short-term LUFS (3 s),
  power-averaged; a stem in the mix is its own LUFS plus its gain. Lead over
  accompaniment per motif phrase, 808+bass / drums / bed against the lead,
  low band against mid, music against the film sound per zone, silent
  stretches > 20 s. Writes `check.json`.
- **fit** (`--fit`) — moves mix gains toward the check's targets (lead +3..+9
  dB, 808+bass −6, drums −3, low/mid −7..−5, film zones), then the loudness:
  song −14 LUFS-I, film underlay −20, through the limiter input. Stems are
  never re-rendered for it.

Same toml → the stem is reused, so a re-run after a gain change takes
seconds.

## Outputs

`score.mid` (type 1, 480 PPQ; track 0 is the tempo map, one track per part
named like `lead-wave14`; GM drums on channel 10: 36 kick, 35 soft kick, 38
backbeat, 39 clap, 42 hat, 46 open hat, 45/47/48 toms, 49 crash; programs in
`instruments.toml`), `stem_*.toml|wav`, `score.toml`, `score.wav`,
`score-dry.wav` (no sidechain), `form.json`, `score.json`, `mix.json`,
`check.json`, `score.md` (form, readings, section table), `demo60.mp3`
(verse + second chorus + bridge, 20 s each, crossfaded, −16 LUFS).

## Configuration

Presets, stage chains, role gains, kits, GM maps and seeds live in
`src/sdsp_score/instruments.toml`. Override the file with
`SDSP_SCORE_INSTRUMENTS`; paths with `SDSP_SAMPLES`, `SDSP_FFMPEG`,
`SDSP_CHAIN` (default: `sdsp-chain` on PATH, then
`~/.cargo-target/release/sdsp-chain`; build it with
`cargo build --release -p sdsp-chain`).

## Refusals — exit 2, before any audio

No sdsp-chain (named), no ffmpeg, an unknown kit, a missing drum or 808
sample, a theme without chords or motif, an unknown key or chord, a tempo
(theme, section, tempo map) outside 40..200, an unknown form section or bars
not a multiple of 4, a bed without key/tempo, an ffmpeg without rubberband
for the bed. Exit 1: a silent stretch over 20 s in the render.

## Tests

```bash
uv run pytest -q && ruff check . && ruff format --check .
```

`test_film_form.py` is video-analyzer's 31 film-score tests ported onto
form.json; `test_song_form.py` the song form; `test_midi.py` the file;
`test_check.py` reads synthetic stems of known level and drives `fit`;
`test_cli.py` every refusal, each with an sdsp-chain spy that must stay
uncalled. The film path was checked against the old engine on a real plan
(`la-day1.otio` → `otio_to_form.py` → here): identical notes, every part.

## In video-analyzer

`theme_score.py` / `theme_music.py` / `theme_drums.py` stay there for the
films until they move to `otio_to_form.py` + `sdsp-score`. `score_gen.py`
and `score_film.sh` are **deprecated** (one lead pattern per chord, one
texture end to end) but kept: `radio_scrub`, `music_upbeat` and skills
still call them.
