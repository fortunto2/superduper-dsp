# SuperDuper LinEq

Linear-phase 3-band FIR mastering EQ. Same target curve as the
parametric [EQ](../superduper-eq/README.md) but with **zero phase
distortion** — no transient smearing, no phase rotation on bus.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes |
| Sidechain | no |
| Latency | ~21 ms (1024 samples @ 48 kHz, reported via CLAP) |

## How it works

1. Build a target magnitude curve from the user's RBJ biquad params
   (same maths as the parametric EQ)
2. iFFT the magnitude → symmetric (linear-phase) impulse response
3. Hann-window to a 2048-tap FIR
4. Apply via direct circular-history convolution per sample

Rebuilds FIR off the audio hot path when params drift. CLAP `latency`
extension reports the delay so REAPER's PDC keeps parallel buses
aligned.

## Parameters

Same layout as parametric EQ:

| Group | Params |
|---|---|
| Low | Freq, Gain |
| Mid | Freq, Gain, Q |
| High | Freq, Gain |
| Output | Output |

## When to use LinEq vs EQ

- **LinEq** — mastering. Surgical cuts that don't pre-ring or smear
  transients. Linear-phase preserves the time-domain shape.
- **EQ (parametric)** — tracking, mixing. Lower latency (0 vs 21 ms).
  Phase-rotation can actually help "glue" — minimum-phase is more
  natural on individual sources.

## Linear-phase tradeoffs

- **Pro** — no phase distortion, parallel bus stays in phase, no
  transient time-smearing
- **Pro** — A/B against EQ is a fair magnitude-only comparison
- **Con** — 21 ms latency (PDC handles it for offline, but live
  monitoring needs lookahead)
- **Con** — pre-ringing on sharp cuts (inherent to symmetric FIR; only
  audible on transients at high gain)

## DSP details

Implementation: `synth_core::linphase::design_linear_phase_fir` (iFFT
→ window → store) + `DirectFirConvolver` (RT-safe circular history).
