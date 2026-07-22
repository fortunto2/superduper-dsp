# sdsp-mash — headless mashup engine

Aligns the beat stems of one song with the vocal of another on a shared BPM
grid — time-stretching the beat to fit, phase-aligning the vocal onto the
kick, cleaning the vocal with a high-pass + compressor, sidechain-ducking the
melodic bed under the vocal, opening an intro lowpass sweep, running a
SuperDuper master chain — and renders a stereo WAV with per-stage LUFS / dBTP.
No REAPER, no DAW — one CLI process, the same CLAP DSP the DAW would load.

Sibling of [`sdsp-chain`](../sdsp-chain): sdsp-chain masters one finished mix;
sdsp-mash *builds* the mix from stems first, then masters it with the same
plugin machinery.

## Usage

```bash
cd /Users/rustam/Music/1music/superduper-dsp

# Render a mashup:
cargo run --release -p sdsp-mash -- <mash.toml> <output.wav>

# Analyse source files (BPM + peak/RMS) to fill in a mash.toml:
cargo run --release -p sdsp-mash -- analyze <wav>…
```

Stems are named inside the config (unlike sdsp-chain, which takes an input WAV
on the command line). Two worked configs with real paths ship here:
[`example.toml`](example.toml) (Jump Around × Агент, matched tempo) and
[`example2.toml`](example2.toml) (Firestarter × Организация, beat stretched to
a 145 grid).

```bash
cargo run --release -p sdsp-mash -- tools/sdsp-mash/example.toml out.wav
```

## `analyze` subcommand

Estimates tempo (autocorrelation of an onset envelope, 0.01 BPM resolution,
60–190 range) and level for each WAV, printing the ×2 / ÷2 octave candidates
so you can resolve half-/double-tempo ambiguity yourself:

```
── analyze: 03. Firestarter/drums.wav ──
  44100 Hz   222.7 s   2 ch
  BPM  71.00   (strength 0.78)
  octave candidates:  ÷2 = 35.50 (0.92)   ×2 = 142.00 (0.59)
  peak   -0.1 dBFS   RMS  -17.4 dBFS
```

(Here the beat is really 142 — the ÷2 candidate scores highest because
Firestarter is a half-time groove. Pick with your ears + the candidates.)

## Typical source: demucs stems

Split each source song into stems (drums / bass / other / vocals):

```bash
demucs -n htdemucs "Some Song.mp3"      # → separated/htdemucs/Some Song/*.wav
```

Then take the beat roles from one song and the `vocals.wav` from another.

## Config — `mash.toml`

```toml
bpm = 105.5              # shared grid tempo
# sample_rate = 44100    # optional; inherited from the first stem if omitted

# One [[track]] per stem placed on the grid.
[[track]]
path = "…/htdemucs/House of Pain - Jump Around/drums.wav"
role = "beat-drums"      # beat-drums | beat-bass | beat-other | vocal
gain_db = 0.0            # level trim (default 0)
offset_beats = 0.0       # where it starts on the grid, in beats (default 0)
start_sec = 0.0          # trim the *source* head (default 0)
# len_sec = 60.0         # optional source length after start_sec (default: to EOF)
# tempo_ratio = 1.0      # WSOLA stretch (beats only; ratio = old_bpm/new_bpm)

[[track]]
path = "…/htdemucs/Oxxxymiron - Агент/vocals.wav"
role = "vocal"
offset_beats = 32.0      # drop the verse in near bar 8 (8 bars × 4 beats)
start_sec = 10.4
auto_align = true        # snap onto the beat-drums (±half-bar) by onset xcorr
highpass_hz = 120.0      # per-track RBJ high-pass (clears rumble/plosives)
# per-track compressor (soft-knee, stereo-linked) — evens out the rap:
comp = { threshold_db = -18.0, ratio = 3.0, attack_ms = 5.0, release_ms = 90.0, makeup_db = 2.0 }

# Sidechain duck — the vocal keys a compressor on the beat-other bus only.
[duck]
threshold_db = -30.0
ratio = 4.0
attack_ms = 8.0
release_ms = 140.0
# knee_db = 6.0          # optional soft-knee width (default 6)

# Intro lowpass sweep on the beat bus — opens over the first N bars (4/4).
[intro_sweep]
bars = 8
from_hz = 350.0
to_hz = 20000.0

# Master chain — identical stage format to sdsp-chain's [[stage]].
[[master]]
plugin = "eq"
params = { "7" = 30.0, "6" = 1.5 }

[[master]]
plugin = "compressor"
params = { "0" = -18.0, "1" = 2.0, "2" = 30.0, "3" = 200.0, "5" = 2.0 }

[[master]]
plugin = "limiter"
params = { "0" = 6.0, "1" = -1.0 }
```

### Roles and how they're processed

| Role | In the mix | Ducked by vocal? | Intro sweep? |
|---|---|---|---|
| `beat-drums` | yes | no | yes |
| `beat-bass` | yes | no (holds the low end) | yes |
| `beat-other` | yes | **yes** | yes |
| `vocal` | yes | is the sidechain **key** | no |

Drums punch through and bass fills the bottom under the vocal; only the melodic
`beat-other` bed is pulled down so the rap sits on top. Multiple stems of the
same role are summed.

### Time-stretch (`tempo_ratio`)

WSOLA (waveform-similarity overlap-add) — pitch-preserving, pure Rust, applied
before placement. **`ratio = old_bpm / new_bpm`**: to pull a 142 BPM beat onto
a 145 grid, `tempo_ratio = 142/145 ≈ 0.979` (output is shorter/faster). Range
0.25–4.0.

**Vocals are never stretched** (formant/pitch artefacts kill the recognisability
that makes a mashup work) — a `tempo_ratio ≠ 1.0` on a `vocal` track is a hard
config error. Stretch the beat under the vocal instead. Pick sources within a
few percent, or a clean 1:1 / 2:1 tempo relationship.

### Phase auto-align (`auto_align`)

On a `vocal` track, cross-correlates the vocal's onset envelope against the
beat-drums bus in a ±half-bar window around `offset_beats` and snaps to the
best-matching lag (coarse 50 Hz envelope search, then a sample-accurate
refinement). The shift is printed in ms.

It only commits the shift when the correlation is convincing (≥ 0.30). A rap
acappella over a *foreign* beat has genuinely weak onset correlation — there is
no "true" phase lock to find — so the shift is **rejected and the nominal
offset kept**, reported as `rejected (low corr) — kept nominal`. Auto-align
earns its keep when the vocal shares rhythmic content with the beat (e.g. it was
already tracked to a similar groove); otherwise dial `offset_beats` by hand.

### Master plugins

Reuses sdsp-chain's host machinery. Supported `plugin =` values:
`eq`, `lineq`, `compressor`, `saturator`, `limiter`, `midside`, `filter`.
Params are keyed by CLAP param ID (string-encoded integer) → float, exactly as
in sdsp-chain — read `effects/<crate>/src/lib.rs` `const PARAMS` for the IDs.
Omit `[[master]]` entirely to render the raw pre-master bus.

## Sections, transitions & FX (megamix / cypher)

Beyond the flat `[[track]]` list, a megamix is authored as `[[section]]`s — each
its own **tempo island** joined by a `transition`, plus timeline `[[fx]]`. See
[`megamix.toml`](megamix.toml) for a full worked example.

```toml
[[section]]
name = "Voodoo × Агент"
start_sec = 52.0            # placement on the global timeline (or start_beat)
bpm = 161.0                # section-local grid — the vocal's tempo
transition = "drop"        # how we enter from the previous section
xfade_beats = 8            # build / crossfade length (in this section's bpm)

[[section.track]]          # the beat: stretched to the section bpm
path = "…/Voodoo People/drums.wav"
role = "beat-drums"
tempo_ratio = 0.9255       # = native_beat_bpm / section_bpm  (149 → 161)
len_sec = 58.0             # ⚠️ cap the stretch input (see below)
# … bass, other …
[[section.track]]          # the vocal: never stretched, must ≈ section bpm
path = "…/Агент/vocals.wav"
role = "vocal"
offset_beats = 8           # relative to the section start, in section bpm
auto_align = true
```

**Why per-section tempo:** vocals aren't stretched, so a vocal and its beat must
share a grid. Real acappellas (110–161 BPM) and real Prodigy beats (130–149 BPM)
never coexist on one grid — so each section runs at the vocal's tempo and the
beat is stretched to meet it (`tempo_ratio = native / section_bpm`).

**Transitions** (`transition =`) — every seam gets a lead-in/lead-out fade
(default 1 beat, override with `lead_in_beats` / `lead_out_beats`) plus a 22 ms
anti-click micro-fade floor on all stems:

| value | effect |
|---|---|
| `crossfade` / `bass_swap` / `filter_sweep` | equal-power overlap; new beat fades in, old fades out (`filter_sweep` adds an opening lowpass) |
| `drop` | hard cut-in + an auto-**riser** over `xfade_beats`, plus an auto beat-repeat lead-out on the outgoing beat |
| `cut` | stop-time, softened: the outgoing beat gets an auto beat-repeat stutter + a closing lowpass over the last beat + an echo tail (fb 0.4) ringing into the pause |
| `breakdown` | old beat filters + fades down over 2 beats, new beat drops in (vocal carries the gap) |
| `fade_except` + `keep = vocal\|hats\|melody` | everything fades except the kept role (`hats`→drums, `melody`→other) |

**Section loudness balance** (`balance_sections`, default **on**): each
section's whole energy (beat + vocal) is levelled to the median section level,
so a section grabbed from a louder part of its source doesn't stick out. Set
`balance_sections = false` (before the `[[section]]` tables) and hand-trim with
per-track `gain_db` when the master limiter's density response needs manual
control (e.g. a hot beat-only finale).

**Timeline FX** (`[[fx]]`, applied to the pre-master mix at `at_sec` or `at_beat`):
`tape_stop` (resampling power-down), `beat_repeat` (accelerating 1/2→1/4→1/8
downbeat stutter), `echo_out` (feedback-delay tail), `kick_pump` (grid-locked
sidechain pump), `riser` (white-noise build with opening high-pass). Optional
params: `len_beats`, `from_hz`, `to_hz`, `feedback`, `delay_ms`, `depth_db`,
`release_ms`, `peak`.

> ⚠️ **Always put `len_sec` on stretched beat stems.** WSOLA runs on the whole
> decoded stem *before* section trimming — without `len_sec` it stretches a
> 6-minute file to render a 50-second section, which takes minutes.

## Output

Stereo 32-bit-float WAV at the project sample rate, plus per-stage analysis on
stdout:

```
Per-stage analysis (LUFS-Integrated + True-Peak):
  [     premaster]  LUFS-I  -13.08   TP    2.8 dBTP   RMS 0.2437
  [         1. eq]  LUFS-I  -13.03   TP    3.7 dBTP   RMS 0.2412
  [ 2. compressor]  LUFS-I  -15.80   TP    0.4 dBTP   RMS 0.1774
  [    3. limiter]  LUFS-I  -11.29   TP   -1.0 dBTP   RMS 0.3034
```

## Limitations

- **No resampler.** Every stem must be at the project sample rate; a mismatch
  errors out with the offending file. (Time-stretch is BPM-only; it does not
  change sample rate.)
- **4/4 assumed** for `intro_sweep` bar → sample conversion and the auto-align
  ±half-bar window.
- **No de-esser** in the per-track vocal FX yet (high-pass + compressor only);
  chain the SuperDuper Vocal plugin upstream or add a master `vocal` stage.
- WSOLA is tuned for beat stems; very large stretch factors on tonal material
  will soften transients.

## Design

- `config.rs` — `mash.toml` parsing + validation (roles, tempo_ratio policy,
  grid math).
- `stretch.rs` — WSOLA time-stretch (pitch-preserving, pure Rust).
- `onset.rs` — onset-strength envelopes (rectified-diff RMS) for align + tempo.
- `align.rs` — phase auto-align (coarse 50 Hz + fine sample-rate onset xcorr).
- `tempo.rs` — BPM estimation (onset autocorrelation + octave candidates).
- `track_fx.rs` — per-track high-pass + soft-knee compressor.
- `duck.rs` — sidechain ducker (envelope follower + soft-knee gain computer).
- `sweep.rs` — exponential intro lowpass sweep on the beat bus.
- `mix.rs` — pure in-memory align + sum-by-role + duck + sweep engine.
- `wav_io.rs` — stereo WAV decode/encode (synth-core only ships a mono writer).
- `render.rs` — bridges config → decoded/stretched/FX'd/aligned stems → bus.
- `analyze.rs` — the `analyze` subcommand.
- `chain.rs` — master chain hosting the statically-linked CLAP plugins.
- `main.rs` — CLI dispatch + per-stage LUFS/dBTP measurement.

### Why WSOLA, not signalsmith-stretch

The `signalsmith-stretch` / `ssstretch` crates are C++ bindings — they pull in
`bindgen` + `libclang` + a C++ toolchain + `dasp` (41 crates). To keep the tool
hermetic and pure Rust (and not endanger the Windows CI path), the beat stretch
is a hand-rolled WSOLA, which is plenty for drum/bass/other stems.

### Why the plugin host lives in the binary

`chain.rs` (which links CLAP) is a binary module, not a library target, on
purpose: a sibling `[lib]` splits clack's feature resolution under the test
profile and duplicates `clack-plugin`. Keeping the plugin host in the bin and
the pure DSP engine testable in-place avoids it.

## Tests

```bash
cargo test --release -p sdsp-mash
```

Covers: the ducker (silence → unity, loud key → expected reduction, attack/
release shape); WSOLA (exact ratio-scaled length, identity at ratio 1, click
spacing scales by the ratio); auto-align (recovers an injected sub-ms shift,
corrects a misconfigured offset); onset envelope peaking on a transient; BPM
detection of synthetic 120 / 145 tracks; per-track high-pass + compressor;
sample-accurate offset math; config parsing (roles, vocal-stretch rejection,
range check, FX/align fields); the sweep cutoff curve; and stereo WAV round-
trip. Integration tests (`integration_tests` module) write synthetic stems to
disk and render them through the real decode → align → duck path, asserting the
vocal lands on the exact grid sample and that ducking lowers beat energy under
the vocal.
