# SuperDuper Harmonic Clean

A **pitch-locked harmonic comb denoiser**, built to clean a **piezo / electric
kubyz** (jaw-harp, khomus). A contact pickup on a metal reed grabs the musical
harmonics *and* a layer of inharmonic micro-rustle — finger/contact noise, reed
buzz, pickup hiss — that sits between the harmonics. This plugin keeps the
harmonics **and the plucks**, and rejects the noise between them.

Put it first in the chain, right after the pickup.

## How it works

A pitched signal repeats every period `T = sr / f0`. Its harmonic content is
(near) identical period to period; inharmonic noise is uncorrelated period to
period. So the plugin:

1. **Tracks f0** on the mono sum with a YIN pitch tracker
   (`synth_core::pitch::YinPitchTracker`, range 40–600 Hz).
2. Runs a **period-synchronous comb** per channel — it averages the input with
   delay-line taps at `T, 2T, …, (K-1)T`:
   `h[n] = (x[n] + x[n-T] + … + x[n-(K-1)T]) / K`. Periodic content adds
   coherently (`h ≈ x` at every harmonic), noise averages down (÷√K). The
   output subtracts a fraction of the between-harmonic residual:
   `out = x − Amount·(x − h)`. **Time-domain, zero added latency — no FFT**, so
   it's fit for a live instrument.
3. **Preserves plucks** with an onset detector (fast vs slow envelope): on an
   attack the comb depth drops so the raw transient passes through clean and
   fast instead of being smeared. On silence/unvoiced the comb is bypassed.
4. **Combines the taps by MEDIAN (default), not mean.** A *mean* comb re-injects
   each pluck one period later (the tap at `T` still holds it, where there's no
   onset to protect it) — an inharmonic echo at `T, 2T…` that on transient-heavy
   contact-pickup material (piezo rustle *is* clicks) actually *adds*
   between-harmonic noise. The **median** discards that single-tap outlier — the
   pluck lives in one tap, the periodic content is the median across taps — so it
   cleans the hiss and never echoes the plucks. `Mode = Mean` is available for
   pure steady hiss (~2 dB better there, but it echoes transients).

## Parameters

| Param | What it does |
|---|---|
| **Amount** | Between-harmonic rejection depth (0 = bypass, 1 = max noise cut). |
| **Bandwidth** | Keep-width around each harmonic → comb tap count `K` (2..8). Low = narrow = **aggressive**; high = wide = **gentle**. |
| **Transient** | Attack preservation — how far the comb re-opens on plucks. Up = sharper plucks. |
| **Mix** | Dry/Wet. |
| **Output** | Output trim (dB). |
| **Range** | Lowest fundamental to lock (Hz) — raise it if the tracker chases an octave-down ghost. |
| **Mode** | Median (default, rejects transient echo — best for piezo clicks) or Mean (classic average, ~2 dB better on pure hiss). |

The GUI shows the live detected **f0**, a **noise-cut meter**, and the output
spectrum so you can watch the harmonics stand out from the lowered noise floor.

## Presets

Kubyz Clean · Gentle / Transparent · Aggressive · Transient Max.

## Measured

On a synthetic kubyz scene (90 Hz drone + broadband noise + plucks): stationary
between-harmonic noise **−7.4 dB** (median mode) at Amount 0.9 / Bandwidth 0.3,
harmonic energy essentially untouched (**Δ −0.1 dB**), pluck peak preserved
**100 %** at Transient 1. On a single-pluck test the median comb's echo is
**−19.3 dB** lower than the mean comb's — i.e. the echo is effectively gone.

## Build

```bash
./scripts/build_harmonic_bundle.sh          # builds + installs the .clap
cargo test --release -p superduper-harmonic  # dsp_smoke + clap_e2e + denoise
```
