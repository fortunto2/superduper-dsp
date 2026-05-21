# SuperDuper Supermass

Valhalla Supermassive-style cascade — reverb (35 m / 15 s) → stereo
chorus → reverb (50 m / 28 s). Massive, modulated, dreamy tails.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes |
| Sidechain | yes (Ducker on wet) |
| Latency | 0 samples |

## What it does

Long-tail texture reverb. Two large reverbs in series with a stereo
chorus between them. Output is naturally wide and modulated. Tail can
sustain up to ~28 s. Use for pad atmospherics, vocal washes, ambient
drone beds — not for natural-room emulation.

## Parameters

| Param | What |
|---|---|
| Mix | Wet/dry |
| Width | Stereo spread |
| Drive | Soft-clip on the wet path |
| (Tilt, Duck Amount, Duck Attack, Duck Release) | Tone + sidechain duck |

## DSP

Built on fundsp `Net` — built once at `activate()`, geometry never
changes. Mix / Width / Drive / Tilt are post-process knobs on top of
the static Net.
