# SuperDuper MidSide

Stereo width + Mid/Side per-channel gain via L-R sum/difference
matrix. Three modes: in-place Width, Encode →, ← Decode.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes |
| Sidechain | no |
| Latency | 0 samples |

## Modes

| Mode | Input | Output | Use |
|---|---|---|---|
| **Width** | L/R stereo | L/R stereo | In-place — change width / mid gain / side gain on a stereo signal |
| **Encode →** | L/R | L=Mid, R=Side | Insert before a mastering chain — let stereo plugins act on M/S |
| **← Decode** | L=Mid, R=Side | L/R | Insert after Encode — convert back to normal L/R |

## Width

```
Mid  = (L + R) × 0.5
Side = (L - R) × 0.5
L'   = Mid × MidGain + Side × SideGain × Width
R'   = Mid × MidGain - Side × SideGain × Width
```

- **Width = 0** → mono (Side suppressed)
- **Width = 1** → original stereo
- **Width = 2** → exaggerated stereo (Side doubled)

## Parameters

| Param | Range | Default |
|---|---|---|
| Mode | Width / Encode / Decode | Width |
| Width | 0..2 | 1 |
| Mid Gain | -12..+12 dB | 0 |
| Side Gain | -12..+12 dB | 0 |
| Output | -12..+12 dB | 0 |

## Encode/Decode workflow

```
[Audio In]
   │
   ├─ MidSide Mode=Encode →
   │     L (Mid)  →  EQ / Comp on the centre
   │     R (Side) →  EQ / Comp on the sides
   │
   └─ MidSide Mode=Decode ←
   │
[Audio Out]
```

The processors between Encode and Decode see L=Mid and R=Side
channels, so any plugin operating on L/R independently effectively
operates on M/S.
