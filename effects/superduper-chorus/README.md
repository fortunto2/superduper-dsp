# SuperDuper Chorus

Multi-tap modulated delay with band-named factory presets — from
Joy Division Atmosphere to Cocteau Twins shimmer to Vangelis Blade
Runner CS-80 lushness.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes |
| Sidechain | no |
| Latency | 0 samples |

## Parameters

| Param | Range | Default |
|---|---|---|
| Rate | 0.05..8 Hz | 0.4 |
| Depth | 0..1 | 0.5 |
| Delay | 1..30 ms | 12 |
| Spread | 0..1 (per-tap phase offset) | 1 |
| Width | 0..1 (stereo) | 1 |
| Feedback | -0.95..+0.95 | 0 |
| Mix | 0..1 | 0.5 |

## Presets

Each preset is a band-character snapshot:

- **Joy Division Atmosphere** — slow LFO, wide width, classic post-punk
- **Cocteau Twins Shimmer** — bright, high feedback, ethereal
- **Vangelis CS-80** — deep / lush / orchestral pad chorus
- **Eno Dimension** — subtle width without obvious modulation
- **Tom Petty 12-string** — fast LFO + shallow depth = 12-string guitar

## DSP

- 4 modulated delay taps per channel, LFOs phase-offset by `Spread`
- Linear interpolation on the taps (fast, mild aliasing — chorus
  hides it)
- Optional feedback for chorus → ensemble territory
