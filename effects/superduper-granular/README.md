# SuperDuper Granular

A live granular cloud. The input is chopped into hundreds of short windowed
fragments and reassembled into a texture — and with one button that texture stops
needing the input at all.

## Freeze is the plugin

Everything else here is shaping. **Freeze** stops the recording, and the cloud
keeps chewing the last few seconds forever: sing one note, hit Freeze, and you
have an endless pad made of your own voice — no sampler, no loop points, no
crossfade to hide. Map it to a sustain pedal (CC 64) and you can catch a moment
mid-phrase with your foot while both hands stay busy.

## How it works

The input streams into a 6-second ring buffer. A scheduler spawns grains at
`Density` per second; each grain reads from somewhere behind the write head
(`Position` back, randomised by `Spray`), lasts `Size` ms, and gets its own pitch
(`Pitch` ± `Jitter`), pan (`Spread`), direction (`Reverse`) and window (`Shape`).

`Density × Size` is the **overlap** — the number that decides what you hear:

| Overlap | Result |
|---|---|
| below 1 | gaps between grains — rhythm, stutter, pointillism |
| 1–4 | a grainy, textured version of the source |
| above 4 | a continuous wash; individual grains stop being audible |

Output is normalised by √overlap, so raising Density thickens the texture instead
of just getting louder.

## Parameters

`Density` — grains per second (or, with `Sync` on, one grain per grid division).
`Size` — grain length in ms.
`Spray` — randomises each grain's start position across the buffer. 0 = every
grain from the same place (coherent, pitched); 1 = scattered (a smear).
`Position` — how far behind the write head grains start. Small = tight and
stuttery; large = a long echo of the past.
`Pitch` / `Jitter` — transposition, and how much it scatters per grain.
`Spread` — stereo placement range.
`Reverse` — probability that a grain plays backwards.
`Freeze` — stop capturing.
`Feedback` — the cloud's output written back into the buffer, so grains
granulate grains. The source dissolves into texture over a few seconds. A DC
blocker sits in that path so the loop can't build up an offset.
`Shape` — grain window: **Hann** (smooth, never clicks), **Tukey** (flat middle,
so each grain keeps its own attack — sampler-ish), **Perc** (instant attack +
exponential decay — pointillist).
`Sync` — spawn on the host grid instead of a free rate. With small Spray and
Position this becomes a tempo-locked beat-repeat.
`Mix` / `Output` / `Preset`.

## MIDI CC

| CC | → |
|---|---|
| 64 (sustain pedal) | Freeze |
| 74 | Density |
| 71 | Size |
| 73 | Feedback |
| 76 | Jitter |
| 1 (mod wheel) | Position |

## Chain tips

- Granular → **Supermass** or **Reverb** is the classic ambient move.
- **Stretch** → Granular → **Formant**: Stretch makes the endless bed, Granular
  adds motion to it, Formant makes it pronounce vowels.
- Kubyz through Granular with `Pitch −12` gives a sub layer that breathes.
- The grain pool is 96 voices. When it's full the scheduler **skips** a spawn
  rather than stealing a sounding grain — stealing would click, and a missing
  grain in a cloud of ninety is inaudible.

## Tests

```bash
cargo test --release -p superduper-granular
```

`dsp_smoke` asserts no clicks (max sample step < 0.4 on a dense cloud), that
Freeze sustains with zero input while the unfrozen cloud correctly decays once
the ring fills with silence, that ±12 st moves the spectral centroid, and that 6×
density stays within 9 dB.
