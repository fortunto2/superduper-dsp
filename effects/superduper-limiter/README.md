# SuperDuper Limiter

Lookahead brickwall limiter with 4× true-peak detection, dithering,
and live GR meter. Final stage of a mastering chain.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes (linked detector) |
| Sidechain | no |
| Latency | up to 5 ms lookahead (reported via CLAP) |

## What it does

Brickwall limiter — hard cap at Ceiling (default -1 dBTP). Threshold
sets where compression starts pulling back; the difference between
Threshold and Ceiling is the headroom budget. Lookahead lets the
detector see transients before they arrive and start releasing
before the peak so there's no clipping at the output.

## True peak

When **True Peak** is ON, the detector upsamples the signal 4× via
linear interpolation and finds inter-sample peaks invisible to the
raw sample stream. Critical for masters destined for lossy codecs
(Spotify, Apple Music) where re-encoding adds another ~0.5 dB of
inter-sample peak overhead.

## TPDF dither

**Dither** ON adds ±0.5 LSB triangular-PDF noise at the 16-bit
quantisation level. Use when bouncing to 16-bit WAV — kills truncation
distortion on quiet tails.

## Parameters

| Param | Range | Default |
|---|---|---|
| Input | -24..+24 dB | 0 |
| Ceiling | -3..0 dBTP | -1 |
| Threshold | -24..0 dB | -6 |
| Release | 1..500 ms | 50 |
| Lookahead | 0..5 ms | 1.5 |
| True Peak | bool | on |
| Dither | bool | off |

## Workflow

1. Set **Ceiling** to -1 dBTP (Spotify/streaming standard)
2. **Input** to push the signal up into the limiter
3. Watch the GR meter — 2-4 dB peak GR is mastering-clean, 6+ dB
   pumps audibly
4. Adjust **Release** to match the song's pulse (faster on drums,
   slower on ballads)
5. Enable **Dither** if rendering 16-bit
