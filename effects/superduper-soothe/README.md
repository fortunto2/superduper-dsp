# SuperDuper Soothe

Dynamic resonance suppressor — automatically tames spectral peaks
that pop out of vocals, instruments, or full mixes (sibilance, mud,
harsh rolled-r resonances, plosive ring). Filter-bank approximation
of [oeksound Soothe2](https://oeksound.com/plugins/soothe2/) without
FFT.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes (mid-summed envelope, per-channel cuts) |
| Sidechain | no |
| Latency | 0 samples |

## What it does

24 log-spaced bandpass filters (200 Hz..10 kHz by default) measure
the energy in each band. For each band, the baseline is the **mean
of its 4 neighbours' envelopes in dB** — representing how loud this
slice of the spectrum *should* be relative to its surroundings. When
a band's envelope exceeds baseline + Sensitivity, the plugin drops a
dynamic peaking-EQ cut at that band's centre. Cut depth scales with
the excess up to Amount.

## Parameters

| Param | Range | Default | What |
|---|---|---|---|
| Amount | 0..24 dB | 6 | Depth of cuts. 0 = bypass-ish. |
| Sens | -24..0 dB | -6 | How much above baseline before suppression kicks in |
| Q | 2..12 | 5 | Peaking-EQ Q. Higher = narrower / surgical |
| Lo | 100..2000 Hz | 300 | Lowest band centre |
| Hi | 3000..16000 Hz | 10000 | Highest band centre |
| Attack | 0.5..30 ms | 5 | Envelope follower attack |
| Release | 10..500 ms | 80 | Envelope follower release |
| Mix | 0..1 | 1 | Wet/dry blend |
| Output | -24..+24 dB | 0 | Post-stage gain |
| Mode | Soft / Sharp / Hard | Sharp | Suppression ratio (0.4 / 0.7 / 1.0) |

## Modes

- **Soft** (ratio 0.4) — gentle, only the hottest peaks pull. Use for
  mastering / broadband material.
- **Sharp** (ratio 0.7) — default. Leans into peaks but stays musical.
- **Hard** (ratio 1.0) — every dB above baseline = a dB cut. Aggressive
  vocal cleanup.

## Presets

- **Vocal Resonance** — focuses on 3-10 kHz (`s` / `sh` / `r` overtones)
- **Low Mud** — 200-600 Hz, kills boxy resonances on bass / kick
- **Russian Voice** — wider band 1.2-9 kHz, sharper Q, tuned for rolled `r` and hard sibilants
- **Tame Anything** — heavy ratio, full spectrum, for over-bright recordings
- **Master Polish** — wide, gentle, broadband, pulls down random peaks
  that bypass the limiter

## Spectrum readout

Red bars hanging from the top of the spectrum strip show per-band cut
depth in real time. The hottest band gets a floating `Cut <freq> ·
<dB>` label so you can see exactly which frequency is being suppressed
right now.

## Tuning recipe

1. Start with the `Russian Voice` or `Vocal Resonance` preset.
2. If too much body is being shaved → narrow the Lo/Hi range or raise
   Sens (less aggressive trigger).
3. If resonances still leak through → lower Sens (more aggressive
   trigger) or raise Amount (deeper cuts).
4. Higher Q = surgical narrow cuts; lower Q = wider transparent cuts.

## DSP details

- 24 bandpass biquads (constant-skirt-gain) measure per-band envelope
- Asymmetric one-pole follower with separate attack/release
- Per-band baseline = mean of `[i-2, i-1, i+1, i+2]` envelopes in dB
- Peaking-EQ cut filters with gain in dB pulled back on every sample;
  smoothed through the attack/release time-constants so the
  suppression feels musical, not pumping
- ~1% CPU at 48 kHz (~48 biquads per channel)
