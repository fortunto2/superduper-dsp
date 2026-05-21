# SuperDuper Vocal

Vocal cleanup chain — de-esser + plosive killer + hum remover +
de-clicker + body cut, with frequency-tracking sibilance detection
and a 2-band Sub Mode.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes |
| Sidechain | yes (Ext Key input on port 1) |
| Latency | 0 samples |

## What it does

Five independent stages, each toggleable:

1. **De-Esser** — phase-coherent peaking-EQ cut at `Ess Freq` with
   bandwidth controlled by `Ess Range`. When `Ess Track` is on, the
   centre frequency steers between 4.5 kHz (`s`) and 9 kHz (`sh`)
   based on which sub-band carries more energy (Sibalance-style).
2. **Plosive Killer** — sub-200 Hz transient detector → dynamic HPF
   cut when a `p`/`b` pop crosses the threshold.
3. **Hum Remover** — adaptive notch bank on the 50/60 Hz fundamental
   + 5 harmonics. Tame mains hum without notching the voice.
4. **De-Clicker** — short/long envelope ratio detector for mouth
   noise between phrases.
5. **Lo body cut** — peaking-EQ in the 300-3000 Hz region for
   plosive/body resonance reduction.

## Parameters

23 params total. Groups:

| Group | Params |
|---|---|
| De-Esser | Ess Thr, Ess Freq, Ess Amt, Ess Range, Ess Track, Ess Listen |
| Lo Band | Lo Thr, Lo Freq, Lo Amt |
| Plosive | Plos On, Plos Thr, Plos Amt, Plos Freq |
| Hum | Hum On, Hum Freq, Hum Str |
| De-Click | Clk Sens, Clk Amt, Clk Floor |
| Sidechain | Ext Key |
| Output | Output, Mix, Sub Mode |

## Workflow

1. Engage **Listen** to monitor only the sibilance the de-esser cuts.
2. Set **Ess Freq** to the centre of the harshness (4-9 kHz).
3. Lower **Ess Thr** until the GR meter ducks 4-8 dB on loud `s`.
4. Turn Listen off — the cut is in place.
5. **Ess Range** narrows the peaking-EQ Q (1.0 = wide, 0.3 = surgical).

## Sub Mode (2-band de-esser)

When ON, only the de-esser core runs — Plosive Killer, Hum Remover,
De-Click, and Lo body cut are bypassed in the audio thread. Use as the
**2nd instance in a 2-band chain**:

- First instance: full Vocal, any preset (e.g. `Bright Vocal`).
- Second instance below: select preset `Sib 2 (sh)` — Sub Mode ON,
  Ess Freq locked at 8 kHz.

This is FabFilter Pro-DS-style multi-band de-essing — two narrow
peaking-EQ cuts at different sibilance centres, without
double-processing the shared cleanup stages.

## Presets

- **Rap Vocal** — moderate de-ess at 6 kHz, light de-click
- **Bright Vocal** — pop/female, sibilance at 7 kHz
- **Heavy De-Ess** — aggressive for over-bright recordings
- **Click Only** — clean dry takes without touching tone
- **Podcast** — gentle transparent cleanup
- **Sib 1 (s)** — first band of 2-band chain (5.5 kHz, narrow)
- **Sib 2 (sh)** — second band, Sub Mode (8 kHz, narrow)
- **Sib Master** — broadband sibilance on master bus

## Spectrum overlays

The spectrum strip shows colour-coded markers for each stage:

- **Green** dashed line — de-esser cut (Ess Freq, tracked or static),
  with translucent zone showing Ess Range bandwidth
- **Red** — Plosive HPF cutoff + tinted zone below
- **Violet** — Hum fundamental + 5 harmonic markers
- **Cyan** — Lo band centre (only when Lo Amt > 0)

## Sidechain (Ext Key)

When `Ext Key` is on, the de-esser detector listens to the sidechain
input (port 2) instead of the dry signal. Route a separate trigger
track via REAPER's pin connector — useful when the lead vocal is
already heavily processed and the unprocessed take is on a hidden
track.
