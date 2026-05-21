# SuperDuper Compressor

Soft-knee feed-forward compressor with three character curves
(Clean / Pump / Smooth), lookahead, sidechain HPF, external sidechain
input, M/S mode, and an oversampled ceiling clipper.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes |
| Sidechain | yes (external port + internal SC HPF) |
| Latency | 0-2 ms (depends on Lookahead, reported via CLAP) |

## Curves

| Mode | Character |
|---|---|
| **Clean** | Giannoulis-Massberg-Reiss soft-knee — transparent, SSL-style |
| **Pump** | FET 1176 attack character — adds punch on drum bus / parallel |
| **Smooth** | Optical LA-2A-style — slow, gentle, sustained material (vocals, bass) |

## Parameters

| Group | Params |
|---|---|
| Compression | Threshold, Ratio, Knee, Curve |
| Envelope | Attack, Hold, Release, Range, Auto Release |
| Detector | SC HPF, Link, M/S mode |
| Lookahead | Lookahead |
| Output | Makeup, Ceiling, Mix, Oversampling |

## Lookahead

Adds up to **2 ms pre-delay** so the compressor can react before the
transient hits. CLAP `latency` extension reports it so REAPER's PDC
keeps parallel buses phase-aligned.

## Sidechain HPF

Detector-only HPF so kick / sub-bass doesn't pump the comp on a full
mix. Off / 60 / 100 / 150 / 200 Hz. The audio path is untouched —
this only shapes what the envelope follower sees.

## M/S mode

When ON the comp operates in Mid/Side: detector hears the **Mid**
channel, GR applies to both M and S equally. Use to compress the
centre without squashing reverb tails in the sides.

## External sidechain

Port 2 accepts a separate trigger signal. Route a stem from REAPER's
pin connector — useful for ducking bass under kick without polluting
the kick's own dynamics.

## Oversampling

Output ceiling clipper runs at 1× / 2× / 4× to keep aliasing below
~80 dB at high gain reduction. Default 2×.

## Live readouts

- GR meter — current gain reduction in dB
- Pre/post waveform scope (input + output overlaid)
- Static curve preview with the operating point dotted on
