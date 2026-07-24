---
name: sdsp-chain
description: Run a headless mastering / mixing chain of SuperDuper DSP plugins from the CLI — no REAPER, no DAW. Use when the user wants to process a WAV file through a chain of our CLAP effects (EQ → Compressor → Saturator → MidSide → Limiter, etc.) and get per-stage LUFS / dBTP / RMS measurements, build a reproducible mastering pipeline, drive a CI render farm, or A/B mastering recipes. Do NOT use for single-plugin testing (use `sdsp-runner`), for REAPER session work (use reaper-daw skill), or for designing the plugins themselves (use superduper-plugin skill).
---

# sdsp-chain — Headless mastering / mixing chain

`tools/sdsp-chain` in `/Users/rustam/Music/1music/superduper-dsp/` is a CLI that statically links 12 of our CLAP effects and runs them serially over a WAV file. Same DSP as REAPER would load — just in one process with no DAW, no plugin scanning, no GUI.

Per-stage measurement (LUFS-I, dBTP true peak, RMS) is printed to stdout. Output WAV is written to the path you pass.

## Invocation

```bash
cd /Users/rustam/Music/1music/superduper-dsp
cargo run --release -p sdsp-chain -- <config.toml> <in.wav> <out.wav>
```

First build pulls in 12 plugin rlibs + synth-core + clack — takes a few minutes. Subsequent builds are seconds.

## Config format — TOML

Each `[[stage]]` is one plugin in the chain. Stages run in declaration order. Params are keyed by CLAP param ID (string-encoded integer) → float value.

```toml
[[stage]]
plugin = "eq"
params = { "1" = 1.0, "3" = -0.5, "6" = 1.5, "7" = 30.0 }

[[stage]]
plugin = "compressor"
params = { "0" = -18.0, "1" = 2.0, "2" = 30.0, "3" = 200.0 }

[[stage]]
plugin = "saturator"
params = { "0" = 4.0, "1" = 0.0, "5" = 1.0 }

[[stage]]
plugin = "midside"
params = { "1" = 1.15, "2" = 1.0 }

[[stage]]
plugin = "limiter"
params = { "0" = -8.0, "1" = -1.0 }
```

A working example lives at `tools/sdsp-chain/example.toml` — copy and edit it.

## Supported plugins + their param IDs

Param tables live in `effects/<crate>/src/lib.rs` under `const PARAMS: &[ParamDef]`. **Always grep that file** when picking IDs — they're the single source of truth and can drift.

Quick reference (current state, double-check by reading `PARAMS` if uncertain):

| `plugin =` | Crate | Typical params (id → name) |
|---|---|---|
| `eq` | `superduper-eq` | 0 Low Freq · 1 Low Gain · 2 Mid Freq · 3 Mid Gain · 4 Mid Q · 5 High Freq · 6 High Gain · 7 HP · 8 LP · 9 Output |
| `lineq` | `superduper-lineq` | same shape as EQ but linear-phase FIR (~21 ms latency) |
| `compressor` | `superduper-compressor` | 0 Threshold dB · 1 Ratio · 2 Attack ms · 3 Release ms · 4 Range dB · 5 Hold · 6 Knee dB · 7 Curve · 8 Mix · 9 Output |
| `saturator` | `superduper-saturator` | 0 Drive dB · 1 Type (0 Tape / 1 Tube / 2 Soft) · 2 Tone · 3 Output · 4 Mix · 5 OS (0/1/2 = 1×/2×/4×) |
| `limiter` | `superduper-limiter` | 0 Threshold dB · 1 Ceiling dBTP · 2 Release · 3 Lookahead · 4 Output |
| `midside` | `superduper-midside` | 0 Mode (0 Width / 1 Encode / 2 Decode) · 1 Width · 2 Mid Gain · 3 Side Gain · 4 Output |
| `filter` | `superduper-filter` | mastering filter — HP / LP / shelves + Daft-style resonance |
| `reverb` | `superduper-reverb` | Dattorro plate |
| `supermass` | `superduper-supermass` | Valhalla-style cascade |
| `delay` | `superduper-delay` | Lagrange-interp + ping-pong |
| `vocal` | `superduper-vocal` | de-esser (peaking-EQ) + de-clicker + hum / plosive + Sub Mode for 2-band chains |
| `chorus` | `superduper-chorus` | stereo chorus |

**Not currently in sdsp-chain (CLAP-only, would need to be added as path deps in `tools/sdsp-chain/Cargo.toml`):**
- `soothe` (`superduper-soothe`) — dynamic resonance suppressor (24-band filter bank)
- `nam` (`superduper-nam`) — Neural Amp Modeler — needs `~/.superduper-dsp/nam/<model>.nam` available at runtime

Add either by:
1. Adding the path dependency to `tools/sdsp-chain/Cargo.toml`.
2. Adding an `impl_stage!` invocation in `src/main.rs` referencing the
   plugin's `PluginShared` / `Plugin` types.
3. Adding a `match` arm in `dispatch()`.

The pattern is identical to the existing 12 stages.

The list of statically-linked plugins is declared in `tools/sdsp-chain/Cargo.toml` — if a plugin you need isn't there, add it as a path dep and wire up a stage function in `src/main.rs` (use the `impl_stage!` macro pattern).

## What the runner prints

For every stage:

```
[1/5] eq           LUFS-I -18.4   dBTP -2.1   RMS -22.7
[2/5] compressor   LUFS-I -16.8   dBTP -1.9   RMS -19.4
…
```

Plus the final output WAV-file LUFS-I + dBTP. This is the same BS.1770-4 K-weighted meter that ships inside `superduper-spectrum` (`synth-core::loudness`) — calibrated against a 1 kHz sine.

## Typical use cases

**1. Reproducible mastering preset.** Commit the TOML next to the source WAV; CI re-renders and asserts LUFS-I ∈ target range.

**2. Mastering recipe iteration.** Edit the TOML, re-run, compare per-stage LUFS — no DAW round-trip.

**3. A/B between configs.** Render `out_A.wav` and `out_B.wav` with two TOMLs; open in REAPER side-by-side.

**4. Render farm / batch.** Loop over a folder of stems:
```bash
for w in stems/*.wav; do
  cargo run --release -q -p sdsp-chain -- master.toml "$w" "out/$(basename "$w")"
done
```

## Authoring a new chain — recipe

1. **Pick the target LUFS.** Spotify -14, Apple Music -16, YouTube -14, club master -8 to -10. See `MASTERING.md` in the repo for the full table.

2. **Start from `example.toml`** and copy it: `cp tools/sdsp-chain/example.toml my-master.toml`.

3. **Read the param tables.** `rg 'ParamDef' effects/superduper-eq/src/lib.rs` (or whichever plugin) — IDs are positional, so the `&[ParamDef]` order is the ID-to-name map.

4. **Iterate.** Run, inspect per-stage LUFS, adjust thresholds. A typical mastering chain:
   - `eq` — subtractive corrective EQ (cut not boost)
   - `compressor` — 2:1, slow attack (~30 ms), release matched to song tempo
   - `saturator` — 1-4 dB Tape or Tube, 4× OS for clean mastering
   - `midside` — Width 1.05-1.20, Side Gain +1 dB max for stereo glue
   - `limiter` — push threshold until LUFS-I hits target, ceiling -1 dBTP

5. **Validate the output WAV** in REAPER (drag in, compare against original) — the CLI numbers are accurate but ears are the final judge.

## Pitfalls

- **`PARAMS` ordering can change between releases.** A TOML pinned to an old git revision may use stale IDs. Always re-read the param table after a `git pull`.
- **MidSide Mode = 0 (Width)** does in-place L/R → M/S → adjust → L/R. Modes 1 and 2 leave the signal in M/S — only use 1 and 2 if you have an encode/decode pair somewhere in the chain.
- **Saturator OS 0** means 1× (native) — aliasing climbs fast above drive 6 dB. Default to OS 2 (4×) for mastering.
- **Limiter Lookahead** adds latency — for offline rendering that's fine; for live preview the chain runner is offline anyway.
- **Sample rate of the input WAV** is preserved; plugins activate at the file's `sr`. Don't mix SR within a chain.

## See also: sdsp-mash

`tools/sdsp-mash` is the sibling tool for **building mashups**, not mastering a
single WAV. It places the demucs beat stems of one song and the vocal of
another on a shared BPM grid (`offset_beats`), sidechain-ducks the `beat-other`
bus from the vocal, opens an intro lowpass sweep over the first N bars, then
runs a master chain reusing the *same* `[[master]]` stage format as sdsp-chain's
`[[stage]]`. Output is stereo (sdsp-chain folds to mono). Reach for it when the
input is "beat of A + acappella of B" rather than "one finished mix to master".

```bash
cargo run --release -p sdsp-mash -- tools/sdsp-mash/example.toml out.wav
```

v0 has no time-stretch — sources must share a tempo. See
`tools/sdsp-mash/README.md` for the full `mash.toml` schema.

## Where to dig deeper

- `tools/sdsp-chain/src/main.rs` — `impl_stage!` macro + dispatch table
- `tools/sdsp-chain/example.toml` — five-stage example
- `tools/sdsp-mash/README.md` — mashup engine (align + duck + sweep + master)
- `synth-core/src/loudness.rs` — BS.1770 meter implementation
- `MASTERING.md` (repo root) — full mastering recipe + per-platform LUFS targets
- Project `CLAUDE.md` — full plugin list + DSP rules
