# SuperDuper Saturator

Analog-style warmth — Tape / Tube / Soft-tanh saturation with Tone
tilt and 2×/4× polyphase oversampling.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes (independent per-channel state) |
| Sidechain | yes (port, currently unused — placeholder for future dynamic drive) |
| Latency | 0 samples |

## Signal chain (per sample, per channel)

```
in → DcBlock → Drive (linear gain from dB knob) → saturate(curve)
   → Tilt (one-pole high-shelf, ±6 dB at ±1.0)
   → Output gain → Mix with dry
```

## Curves

| Curve | Math | Sound |
|---|---|---|
| **Tape** | Algebraic soft-clip, soft top | Subtle, broad-spectrum warmth |
| **Tube** | Asymmetric, strong 2nd harmonic | Glassy / horn-y / vintage |
| **Soft** | tanh | Classic clean limiter-like |
| **Inflate** | Polynomial loudness curve (Curve −50..+50) | Lifts quiet parts, full scale stays full scale. Its input gain is **Drive − 6 dB**, so the default Drive is unity and a master at −3 dBFS keeps its peaks |

## Oversampling

| OS | What |
|---|---|
| 1× | Off — cheap, aliasing at high drive |
| 2× | Polyphase halfband — good for most cases (default) |
| 4× | Cascaded 2× × 2× — mastering-grade, ≥80 dB aliasing rejection |

## Parameters

| Param | Range | Default |
|---|---|---|
| Drive | 0..+36 dB (Inflate: Drive − 6 dB into the curve) | +6 |
| Type | Tape / Tube / Soft / Inflate | Tape |
| Curve | −50..+50 (Inflate only) | 0 |
| Tone | -1..+1 (low-shelf tilt) | 0 |
| Output | -24..+12 dB | 0 |
| Mix | 0..1 | 1 |
| OS | 1× / 2× / 4× | 2× |

## Tips

- **Master bus** — Drive 1-4 dB, OS 4× → adds warmth without
  audible distortion
- **Bass thicken** — Tube curve + Tone -0.3 → low-mid harmonics
- **Drum bus** — Tape + Drive 6-10 dB + Mix 0.5 → parallel saturation
