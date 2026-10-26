# SuperDuper Denoise

Removes broadband noise or room reverb. A thin CLAP wrapper over
[`superduper-restore`](../../restore), which holds the DSP with zero
dependencies, so video tools run the same algorithms offline
(`superduper_restore::process_offline`).

## Modes

**Mix** (all modes) blends the latency-aligned dry signal back in: 100 % is fully restored.

| Mode | For | Knobs |
|---|---|---|
| **Spectral** | hiss, wind bed, traffic, fan, room tone | Reduction, Threshold, Smoothing, Learn |
| **Dereverb** | a voice recorded in a reverberant room | Amount, RT60 |
| **Neural** | anything that is not a voice: traffic, crowd, keyboard, wind (48 kHz only) | Mix |

**Spectral** is adaptive by default: each FFT bin's noise floor is the minimum
of its smoothed power over the last ~1.5 s, so it needs no learn step and
follows changing noise. Its blind spot is a sound that never stops (a held
note, a drone): that reads as noise and gets pulled down (measured −19.6 dB on
a held 1 kHz tone). Turn **Learn** on during a noise-only passage and off
again: the mean noise spectrum is frozen as the floor, the held tone survives
(0.0 dB) and SNR still rises by 17.6 dB. The profile is not saved with the
session: it is saved in the plugin state (~19 KB) and comes back on reload at
the same sample rate.

**Dereverb** predicts the late tail from the signal 50 ms earlier under an
exponential decay (RT60) and subtracts it. Built for speech: between syllables
it cuts the tail by 13 dB while syllables lose 2.5 dB. Long sustained sounds
are cut too, so keep it off music beds.

**Neural** is RNNoise (Valin 2018) through the pure-Rust `nnnoiseless` crate,
model compiled in, no ONNX or model file. It is trained at 48 kHz and runs only
there; at any other host rate it refuses with an exact passthrough and the GUI
says so (chosen over resampling, which would add latency and artefacts to the
shared delay). Two things measured that decide when to use it:
- with a voice present it is stable: SNR +4 / +9 / +10.5 dB at noise
  amplitude 0.01 / 0.03 / 0.06, across six noise seeds each; between
  syllables the noise drops ~18 dB.
- on noise with **no voice at all** it is unpredictable: the same white
  noise level was cut anywhere from −1 to −46 dB depending only on the seed.
  For hiss under music or a room with nobody talking, use Spectral.

CPU: stereo realtime factor 0.0055 (58 µs per 512-sample block, release,
Apple silicon), ~17 µs per 10 ms frame per channel.

All modes report the same latency (the FFT size: 2048 samples ≈ 43 ms at 48
kHz) through the CLAP `latency` extension, so switching modes never moves the
track.

## Measured (restore/tests/restore.rs, 2026-10-26)

- Spectral adaptive, speech-like bursts in white noise: SNR 11.8 → 25.6 dB (+13.8).
- Spectral learned, held tone: tone kept at 0.0 dB, SNR +17.6 dB.
- Neural, voiced talker in noise: SNR 9.6 → 18.5 dB (+8.9), noise between
  syllables −18.8 dB; through the CLAP host −18.2 dB (Mix 50: −5.6 dB).
- Through the real CLAP host at defaults: steady noise −11.3 dB; `process()`
  allocates nothing, in Spectral and in Neural with Mix.

## Credits

The spectral gating, minimum-statistics tracker, dereverb model, STFT and FFT
are ported from [FilmCraft](https://github.com/storytold/filmcraft)
`filmcraft-audio-dsp` (MIT/Apache-2.0, © 2026 ArtCraft Team and the FilmCraft
contributors). Neural is [nnnoiseless](https://github.com/jneem/nnnoiseless)
(BSD-3-Clause), a Rust port of Xiph's RNNoise. See `restore/NOTICE`.
