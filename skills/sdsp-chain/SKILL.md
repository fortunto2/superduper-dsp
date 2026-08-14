---
name: sdsp-chain
description: Render SuperDuper DSP plugin chains headlessly from the CLI — no REAPER, no DAW. Multi-track mixing, per-stage sidechains, time-varying parameter automation, params by name, per-stage LUFS/dBTP/RMS. Use when the user wants to process WAVs through our CLAP plugins reproducibly (mastering chains, sound-design demos, "voice → kubyz" renders, CI render farms, A/B recipes). Do NOT use for single-plugin audition (use `sdsp-runner`), REAPER session work (reaper-daw skill), or writing the plugins themselves (superduper-plugin skill).
---

# sdsp-chain — headless renderer for plugin chains

`tools/sdsp-chain` in `/Users/rustam/Music/1music/superduper-dsp/` statically links
**15** of our CLAP plugins and renders a whole arrangement from one TOML file. Same
DSP REAPER would load, in one process: no DAW, no plugin scan, no GUI. This is the
engine a future GUI app is meant to sit on.

## Invocation

```bash
cd /Users/rustam/Music/1music/superduper-dsp
cargo run --release -p sdsp-chain -- <config.toml> [<in.wav> <out.wav>]

# Introspection — no need to grep PARAMS tables any more:
cargo run --release -p sdsp-chain -- --list             # every plugin + param count
cargo run --release -p sdsp-chain -- --params formant   # id / name / min / max / default / unit
```

The binary lands at `$CARGO_TARGET_DIR/release/sdsp-chain` (this machine:
`/Users/rustam/.cargo-target/release/sdsp-chain`) — call it directly to skip cargo.

`in.wav`/`out.wav` are optional: a config with `[[track]]` entries carries its own
inputs, and `out = "…"` in the config sets the destination.

## Params are addressed BY NAME, in real units

```toml
[[stage]]
plugin = "formant"
params = { Mode = 1.0, Follow = 1.0, Glide = 22.0, Width = 0.9, Mix = 0.95 }
```

Names are case-insensitive; numeric ids still work. Values are in each param's own
units — Hz, dB, ms, semitones, `gr/s` — the same numbers the plugin GUI shows, **not**
normalised 0..1. An unknown name or an out-of-range value is reported (unknown = hard
error, out-of-range = warning + clamp), so a typo can't silently do nothing.

## Single chain (simple case)

```toml
out = "master.wav"

[[stage]]
plugin = "eq"
params = { "Low Gain" = 1.0, "High Gain" = -0.5 }

[[stage]]
plugin = "limiter"
params = { Input = 6.0, Ceiling = -1.0 }
```

## Multi-track mix

Each `[[track]]` has its own input and chain; the tracks are summed, then `[[master]]`
stages run on the mix.

```toml
out = "render.wav"
tail_s = 4.0                    # extra silence so reverbs/pads/clouds ring out
sidechain = "voice.wav"         # default key for every sidechain-capable stage

[[track]]
name = "kubyz speaking"
input = "kubyz.wav"
gain_automate = [[0.0, -60.0], [8.0, -8.0], [18.0, 1.0]]   # [[seconds, dB], …]

  [[track.stage]]
  plugin = "formant"
  params = { Mode = 1.0, Follow = 1.0, Drive = 0.3 }

  [[track.stage]]
  plugin = "supermass"
  sidechain = ""                # "" = no sidechain for this stage
  params = { Mix = 0.25 }

[[master]]
plugin = "limiter"
params = { Input = 8.0, Ceiling = -1.0 }
```

Track keys: `name`, `input`, `sidechain`, `gain_db`, `gain_automate`, `mute`, `[[track.stage]]`.

## Automation — parameters over time

```toml
[[track.stage]]
plugin = "granular"
params = { Density = 45.0, Size = 240.0, Spray = 0.55 }
# Freeze catches the moment at 12 s and holds it forever after.
automate = { Freeze = [[0.0, 0.0], [11.9, 0.0], [12.0, 1.0]] }
```

Breakpoints are `[[seconds, value]]`, linearly interpolated, clamped outside the
range, applied once per 256-sample block (5.3 ms at 48 kHz). This is the only way to
show anything time-varying in a headless render — Freeze catching a moment, a voice
handing a phrase over, a filter opening.

## Sidechain

`sidechain = "file.wav"` at the top level keys **every** stage that has an input port
1; per-stage `sidechain` overrides it; `sidechain = ""` disables it for one stage.
Stages that have no sidechain input just ignore it.

Plugins with a sidechain input: `compressor`, `reverb`, `supermass`, `delay`,
`formant` (its `Voice` input — this is what Follow mode tracks).

## Plugins

`--list` is authoritative. Currently: `eq`, `lineq`, `compressor`, `saturator`,
`limiter`, `midside`, `vocal`, `filter`, `reverb`, `supermass`, `delay`, `chorus`,
`formant`, `granular`, `stretch`.

## What it guarantees

- **Sample rate comes from the input file.** All inputs must share a rate (a mismatch
  is a clear error, not a silently wrong render).
- **No sample is dropped.** The final partial block is padded and the output truncated
  back to the input length, so output length == input length (+ `tail_s`).
- **Deterministic.** Fixed 256-frame blocks, so a render is identical regardless of
  any host buffer setting.
- Per-stage LUFS-I / dBTP / RMS printed for every stage, plus the mix and master.

## Worked example — "voice → kubyz"

`~/Music/1music/demos/` holds five configs built from a real take (vocal stem + live
kubyz), the clearest being `05_journey.toml`: the voice sings, a `stretch` pad grows
out of it, `formant` in Follow mode makes the kubyz speak the voice's vowels, then the
voice's track gain drops away — the tracker gates, the last vowel freezes and the
instrument finishes the phrase. Three tracks, sidechain, gain automation, master
limiter: a good template for any arrangement-shaped render.

## Adding a plugin

1. `tools/sdsp-chain/Cargo.toml` — add the crate as a dependency.
2. `impl_stage!(stage_<key>, superduper_<key>::SuperDuper<Name>, "co.superduperai.<key>", "/sdsp-chain/<key>");`
3. Add a `PluginSpec` row in `registry()` — key, `PARAMS` table, render fn, and whether
   it has a sidechain input. `--list`, `--params` and dispatch all read that one table.
4. The plugin's `PARAMS` must be `pub`.

## Lessons

- **Don't write a second renderer.** If a render needs something this tool lacks
  (another input, automation, a mixer), extend this tool — that's the point of it.
- `OnePoleLp` used to clamp its cutoff at 20 Hz, which silently disabled every
  sub-20 Hz modulation smoother in the codebase. If a rate control seems inert, check
  what the primitive underneath clamps to.
- Level: chains routinely land 10 dB under a listenable level. The limiter's `Input`
  is the easy lift; check the printed LUFS-I rather than guessing.
