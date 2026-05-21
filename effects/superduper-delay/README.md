# SuperDuper Delay

3rd-order Lagrange-interpolated delay with tape-style feedback
saturation, Stereo / Ping-Pong / Slap modes, and sidechain ducking.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes |
| Sidechain | yes (Ducker on wet) |
| Latency | 0 samples |

## Modes

- **Stereo** — L and R independent
- **Ping-Pong** — repeats bounce L↔R
- **Slap (Haas)** — 25-40 ms single tap, sub-ITD short delay (vocal width)

## Parameters

| Group | Params |
|---|---|
| Time | Time, Width, Time Sync, Time Div, Mode |
| Feedback | Feedback, Tone, Drive |
| Output | Mix |

## Sync

Time Sync ON locks Time to host BPM via the Time Div selector. Free
mode gives milliseconds via the Time knob.

## DSP

- 3rd-order Lagrange fractional taps — sweep Time without aliasing clicks
- Tape feedback path: each repeat softly saturates and darkens via
  a one-pole LPF (Tone control)
- DC blocker before the feedback loop
- Two-pole slew on Time so pitch automation is C¹ continuous (single
  one-pole = audible click on target changes)

## Sidechain

Wet path runs through a Ducker — route the dry signal into the SC
port for "delay only between phrases" effect.
