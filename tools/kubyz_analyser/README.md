# kubyz_analyser

Extracts a 16-harmonic amplitude vector and the three strongest formant
peaks from a recording of a real jaw harp / kubyz / khomus. Spits out a
ready-to-paste Rust snippet for `effects/superduper-kubyz/src/presets.rs`.

## When to use

You record (or download) a clean note of a real kubyz and want the
SuperDuper Kubyz plugin to mimic it. Manually picking 16 harmonic levels
+ 3 formants is tedious; this script does it from the audio.

## Setup (one time)

```bash
# Make sure you use real CPython, NOT the Pyodide one at ~/.local/bin.
/opt/homebrew/bin/python3.13 -m venv .venv
.venv/bin/pip install numpy scipy
# librosa is nice to have but not required for the current script.
```

## Run

```bash
.venv/bin/python tools/kubyz_analyser/analyse.py path/to/recording.wav
```

Defaults to `/Users/rustam/Music/1music/my songs/Media/kubiz1000.wav` if
no path is given.

## What you get

```
file:        path/to/recording.wav
sr=44100  duration=3.251s  peak=0.534  rms=0.0675
f0 ≈ 73.3 Hz  (period 602 samples, MIDI ≈ 38.0)

Harmonics (relative to H1):
  n | f (Hz) | linear |  dB below H1
   1 |   73.3 |  1.000 |   -0.00
   ...

Formant peaks (smoothed envelope):
  F1 ≈  808.8 Hz   (envelope level -64.5 dB)
  F2 ≈ 1091.5 Hz   (envelope level -47.3 dB)
  F3 ≈ 1242.2 Hz   (envelope level -60.4 dB)

Suggested Rust preset (paste into presets.rs):

// from your-recording.wav
// f0 ≈ 73.3 Hz  (MIDI 38.0)
let db: [f32; N_HARMONICS] = [
     -0.00,  33.83, ...
];
// Formant: F1=809 Hz, F2=1091 Hz, F3=1242 Hz
// BW:     bw1=47,  bw2=43,  bw3=48
```

## How to make a new Kubyz preset from it

1. Run the script on the recording.
2. Copy the suggested `let db: [f32; N_HARMONICS] = [...]` block.
3. In `effects/superduper-kubyz/src/presets.rs` add a `harmonics_<name>()`
   helper that returns `db_to_lin_array(db)` for that block.
4. Add a `FormantPreset` const with the F1/F2/F3 and bandwidths.
5. Add a `KubyzPreset { name: ..., harmonics: harmonics_<name>(), formant: ... }`
   entry to the `presets()` table.
6. Rebuild: `./scripts/build_kubyz_bundle.sh`. The new preset shows up in
   the plugin's preset dropdown.

## How the analysis works (very short)

1. **Fundamental (f0)** — autocorrelation on the sustain part of the
   recording, peak in the 40-400 Hz lag range.
2. **Harmonics** — FFT (Hann-windowed), then read the peak amplitude
   inside ±3 bins of each `n * f0` frequency.
3. **Formants** — smooth the magnitude spectrum (50-bin moving average),
   find the three strongest peaks in 200-3500 Hz.
4. **Bandwidth** — width where the smoothed envelope drops 3 dB around
   each formant centre.

The output dB values are positive when the harmonic is louder than H1
and negative when it's quieter. That matches the convention in
`presets.rs::db_to_lin_array`, which converts via `10^(db/20)` then
normalises by the loudest harmonic so the additive sum stays bounded.

## Limits

- Single-note recordings only. Mix down to mono.
- Assumes a steady-state pitch — works well on a sustained note, less
  well on bent / pitch-modulated material.
- Formant detection is envelope-based; tight resonances on single
  harmonics will read as "narrow formants" with the bandwidth equal to
  one harmonic spacing.
