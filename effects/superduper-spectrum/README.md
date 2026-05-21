# SuperDuper Spectrum

Pass-through analyzer + BS.1770 LUFS meter + dBTP true-peak monitor.
The mastering tool: see what's actually happening on the bus.

| | |
|---|---|
| Category | Effect (audio-in → audio-out, identity) |
| Stereo | yes |
| Sidechain | no |
| Latency | 0 samples |

## Views

| Mode | What |
|---|---|
| Spectrum | Instantaneous log-Hz × dB magnitude curve |
| Spectrogram | Time-frequency waterfall (3 colour palettes) |
| Split | Both stacked — top spectrum, bottom waterfall |

## Loudness meters

- **LUFS-M** — momentary (400 ms window, K-weighted)
- **LUFS-S** — short-term (3 s window)
- **LUFS-I** — integrated (gated per ITU-R BS.1770-4)
- **dBTP** — true peak via 4× linear-interp oversampler. Goes red above
  -1 dBTP.

Calibrated against a 1 kHz sine — readings within 0.1 LU of reference.
Update rate ~10 Hz.

## Parameters

| Param | What |
|---|---|
| Mode | Spectrum / Spectrogram / Split |
| Palette | Phosphor / Heat / Mono |
| FFT Size | 256..8192 |
| Smoothing | Inter-frame averaging |
| Tilt | Pre-display ±6 dB tilt |
| Window | Hann / Hamming / Blackman / Rect |

## Use cases

- Mastering chain end — verify LUFS-I + dBTP before render
- Check tonal balance pre-EQ
- Spot resonances on the master bus before they bake into the mix
- Compare A/B versions of a master visually

## DSP

K-weighting = 2-biquad pre-filter (high-shelf + high-pass) before
mean-square integration. Two-stage gating: -70 LUFS absolute + -10 LU
relative to ungated mean. Implementation in
`synth_core::loudness::LoudnessMeter`.
