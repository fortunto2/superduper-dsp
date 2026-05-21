# SuperDuper Filter

Multi-mode resonant filter with Drive (Tanh/Tape/Tube), LFO (free +
tempo sync) and Envelope Follower. Designed for Daft-Punk-style
filter sweeps on the master bus.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes |
| Sidechain | no |
| Latency | 0 samples |

## Modes

| Type | What |
|---|---|
| LP | Low-pass — classic sweep |
| HP | High-pass — top-end pump |
| BP | Band-pass — telephone / radio |
| Notch | Band-reject — pull a resonance out |

Resonance ranges up to self-oscillation. Crank it close to 1 for the
characteristic shriek; modulate `Cutoff` with the LFO or by hand for
build-up filters.

## Drive

Saturator after the filter:

| Mode | Curve |
|---|---|
| Tanh | Soft symmetric — clean overdrive |
| Tape | Asymmetric — 2nd-harmonic warmth |
| Tube | Odd harmonics — honk |

Drive + Resonance interact: high drive softens the resonant peak.

## LFO

- **Shape** — Sine / Tri / Square / Saw / Random S+H
- **Free mode** — Rate in Hz
- **Sync mode** — locks Rate to host BPM via tempo division
  (1/1 down to 1/16t, dotted + triplet variants)

## Env Follow

Sidechain-style envelope from the input signal modulates Cutoff:

- Positive **Env Dpt** opens the filter on transients ("wah" on drum hits)
- **Atk** / **Rel** shape the follower curve

## Parameters

| Param | Range | Default |
|---|---|---|
| Type | LP / HP / BP / Notch | LP |
| Cutoff | 20 Hz..20 kHz (log) | 1000 |
| Reso | 0..1 | 0.3 |
| Drive Mode | Tanh / Tape / Tube | Tanh |
| Drive | 0..1 | 0 |
| LFO Shape | Sine / Tri / Sq / Saw / S+H | Sine |
| LFO Sync | bool | off |
| LFO Rate | 0.05..20 Hz | 0.5 |
| LFO Div | 1/1..1/16t | 1/4 |
| LFO Dpt | 0..1 | 0 |
| Env Dpt | -1..+1 | 0 |
| Env Atk | 0.5..50 ms | 5 |
| Env Rel | 10..500 ms | 100 |
| Output | -24..+24 dB | 0 |
| Mix | 0..1 | 1 |
