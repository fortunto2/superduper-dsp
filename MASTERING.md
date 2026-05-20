# Mastering with SuperDuper DSP

Practical mastering workflow for REAPER — what plugins to chain, in
what order, with what defaults. Numbers below are starting points,
not absolutes — your ears decide.

## The chain (insert in this order on the master bus)

```
1. SuperDuper EQ            — tonal balance
2. SuperDuper Compressor    — glue
3. SuperDuper Mid/Side      — width / centre focus
4. SuperDuper Saturator     — analog warmth
5. SuperDuper Limiter       — brickwall + true-peak
6. SuperDuper Spectrum      — monitor (no processing)
```

## Per-plugin starting points

### 1. SuperDuper EQ
- **HP**: 30 Hz (kill DC + sub rumble)
- **Low Freq / Gain**: 60 Hz, +0.5..+1 dB (kick body warmth)
- **Mid Freq / Gain / Q**: 800 Hz, -1 dB, Q 1.0 (cut "boxy" mids)
- **High Freq / Gain**: 10 kHz, +0.5..+1 dB (air shelf)
- **LP**: off (or 18 kHz for tape vibe)

### 2. SuperDuper Compressor
- **Curve**: Clean
- **Threshold**: -18 dB
- **Ratio**: 2:1
- **Attack**: 30 ms (let transients through)
- **Release**: 200 ms (or "Auto")
- **Range**: -3 dB (cap gain reduction)
- **Sidechain HPF**: 100 Hz (don't let kick pump everything)

### 3. SuperDuper Mid/Side
- **Mode**: Width
- **Width**: 1.0..1.2 (subtle widening; 2.0 = brutal)
- **Mid Gain**: 0..+2 dB (push centre — vocals, kick)
- **Side Gain**: 0..-1 dB (tame busy stereo content)

For **true mid-only processing** (e.g. EQ only the centre):
```
Mid/Side (Mode=Encode →) → Stereo EQ (left=mid, right=side) → Mid/Side (Mode=← Decode)
```

### 4. SuperDuper Saturator
- **Type**: Tape (warmth) or Tube (harmonics)
- **Drive**: 2..4 dB (subtle — too much smears transients)
- **Tone**: 0 (neutral) or +1 (slight brightness)
- **OS**: 2× (mastering grade)
- **Mix**: 100% (full series; for parallel use a send)

### 5. SuperDuper Limiter
- **Threshold**: drive into until target LUFS hit
- **Ceiling**: -1.0 dBTP (Spotify / Apple Music standard)
- **Release**: 50..200 ms (longer = more transparent, shorter = louder)
- **True-Peak**: ON (4× oversampling sidechain detector)
- **Target LUFS**: -14 (streaming), -8 (club master)

### 6. SuperDuper Spectrum
- Monitor only. Watch for:
  - Sub-bass rumble below 30 Hz
  - Harsh peaks 2-5 kHz (de-essing territory)
  - Air balance above 10 kHz
  - Phase scope: no excessive "thinning" from M/S widening

## Daft Punk-style intro/outro filter sweep

Insert **SuperDuper Filter** between Compressor and Saturator with the
"DP Sweep 8-bar (Master)" preset for that classic filter-down
"underwater → bright" transition. Automate cutoff for custom sweep
length.

## Render dialog (REAPER → File → Render)

- **Source**: Master mix (with FX)
- **Bounds**: Entire project / Time selection
- **Sample rate**: 44100 Hz (CD) / 48000 Hz (video) / 96000 Hz (master)
- **Channels**: Stereo
- **Sample format**: 24-bit PCM (preserve dynamics) or 16-bit + dither
- **Normalize**: ☑ Normalize / Limit, target **-14 LUFS-I**, peak
  ceiling **-1.0 dBTP**
- **Resample mode**: Sinc Interpolation (HQ) for SR conversion
- **Dither**: enable for 16-bit output

## LUFS targets by destination

| Platform           | LUFS-I  | True-Peak |
|--------------------|---------|-----------|
| Spotify (loud)     | -14     | -1.0 dBTP |
| Apple Music        | -16     | -1.0 dBTP |
| YouTube            | -14     | -1.0 dBTP |
| Tidal              | -14     | -1.0 dBTP |
| Amazon Music       | -14     | -2.0 dBTP |
| Broadcast (EBU R128)| -23    | -1.0 dBTP |
| Club / vinyl       | -8..-10 | -0.5 dBTP |

## Common mistakes to avoid

- **Limiter pushed too hard** → wave distortion. Aim for ≤ 5 dB GR
  on transient peaks, ≤ 2 dB on sustained material.
- **Master compression too fast** → kills transients. Attack ≥ 20 ms.
- **Width > 1.5 across full range** → phase issues, mono summing
  cancels low end. Widen only above 200 Hz (M/S decode → stereo HP
  on side channel).
- **Saturator drive > 6 dB** → noticeable smear, loss of detail.
- **No HP filter** → sub-rumble eats limiter headroom.

## Parallel processing trick (parallel compression)

```
Master bus
   ├── Send 30% → Aux track [SuperDuper Compressor: heavy crush, +10 dB output]
   └── Direct → main chain
```

Mix the slammed aux back at -20..-15 dB. Adds body + sustain without
killing transients.

## Generating the mastering chain via REAPER MCP

If you have the `reaper` MCP profile loaded, ask Claude:

> Add the SuperDuper mastering chain to the master track of my current
> REAPER project — EQ, Compressor, Mid/Side, Saturator, Limiter,
> Spectrum.

(MCP can't currently target the actual master track — but you can
add the chain to a dedicated "Master Bus" track and route everything
to it.)
