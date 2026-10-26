# SuperDuper Denoise

Removes broadband noise or room reverb. A thin CLAP wrapper over
[`superduper-restore`](../../restore), which holds the DSP with zero
dependencies, so video tools run the same algorithms offline
(`superduper_restore::process_offline`).

## Modes

| Mode | For | Knobs |
|---|---|---|
| **Spectral** | hiss, wind bed, traffic, fan, room tone | Reduction, Threshold, Smoothing, Learn |
| **Dereverb** | a voice recorded in a reverberant room | Amount, RT60 |
| **Neural** | RNNoise via `nnnoiseless` — **not built yet**, passes audio through | — |

**Spectral** is adaptive by default: each FFT bin's noise floor is the minimum
of its smoothed power over the last ~1.5 s, so it needs no learn step and
follows changing noise. Its blind spot is a sound that never stops (a held
note, a drone): that reads as noise and gets pulled down (measured −19.6 dB on
a held 1 kHz tone). Turn **Learn** on during a noise-only passage and off
again: the mean noise spectrum is frozen as the floor, the held tone survives
(0.0 dB) and SNR still rises by 17.6 dB. The profile is not saved with the
session yet.

**Dereverb** predicts the late tail from the signal 50 ms earlier under an
exponential decay (RT60) and subtracts it. Built for speech: between syllables
it cuts the tail by 13 dB while syllables lose 2.5 dB. Long sustained sounds
are cut too, so keep it off music beds.

All modes report the same latency (the FFT size: 2048 samples ≈ 43 ms at 48
kHz) through the CLAP `latency` extension, so switching modes never moves the
track.

## Measured (restore/tests/restore.rs, 2026-10-26)

- Spectral adaptive, speech-like bursts in white noise: SNR 11.8 → 25.6 dB (+13.8).
- Spectral learned, held tone: tone kept at 0.0 dB, SNR +17.6 dB.
- Through the real CLAP host at defaults: steady noise −11.3 dB; `process()`
  allocates nothing.

## Credits

The spectral gating, minimum-statistics tracker, dereverb model, STFT and FFT
are ported from [FilmCraft](https://github.com/storytold/filmcraft)
`filmcraft-audio-dsp` (MIT/Apache-2.0, © 2026 ArtCraft Team and the FilmCraft
contributors). See `restore/NOTICE`.
