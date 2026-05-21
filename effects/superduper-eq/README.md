# SuperDuper EQ

3-band parametric EQ — low shelf + mid peaking + high shelf + HP/LP +
output trim. Classic RBJ biquad cookbook math.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes (independent per-channel biquads) |
| Sidechain | no |
| Latency | 0 samples |

## Bands

| Band | Type | Range |
|---|---|---|
| Low | Shelf | 30..500 Hz |
| Mid | Peaking | 200..10k Hz (Q adjustable) |
| High | Shelf | 1k..16k Hz |
| HP | High-pass | 20..500 Hz |
| LP | Low-pass | 1k..20k Hz |

## Parameters

| Param | Range | Default |
|---|---|---|
| Low Freq | 30..500 Hz | 100 |
| Low Gain | -18..+18 dB | 0 |
| Mid Freq | 200..10000 Hz | 1000 |
| Mid Gain | -18..+18 dB | 0 |
| Mid Q | 0.3..10 | 1.0 |
| High Freq | 1000..16000 Hz | 8000 |
| High Gain | -18..+18 dB | 0 |
| HP | 20..500 Hz (or off) | off |
| LP | 1000..20000 Hz (or off) | off |
| Output | -24..+24 dB | 0 |

## Notes

- Direct Form II Transposed — numerically stable at low Fc / high Q
- No oversampling — colour-correct, minimum-phase, classic EQ
- For linear-phase mastering EQ see [SuperDuper LinEq](../superduper-lineq/README.md)

## Use cases

- Track-level corrective EQ
- Bus carving (mid cut at 250-400 Hz)
- High-shelf air on vocals (+1 dB @ 12 kHz)
- HP 80 Hz on everything that isn't kick/bass
