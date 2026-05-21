# SuperDuper Reverb

Dattorro figure-of-eight plate reverb with modulated allpasses,
sidechain ducking, and Lagrange-3 fractional taps for click-free
SIZE sweeps.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes (genuine cross-coupled stereo) |
| Sidechain | yes (Ducker) |
| Latency | 0 samples |

## What it does

Classic Dattorro plate topology — four delay lines + two pairs of
allpass diffusers + cross-coupled feedback. Tail length up to ~30 s.
The Ducker on the wet path drops the reverb level under sidechain
peaks (typical use: route the dry vocal into the SC port so the
plate ducks under each phrase).

## Parameters

| Group | Params |
|---|---|
| Time | Decay, Predelay, PD Sync, PD Div |
| Tone | Damping, Width, Modulation |
| Action | Freeze (infinite hold) |
| Ducker | Duck Amount, Duck Attack, Duck Release |
| Output | Mix |

## Tips

- **Predelay sync** locks the pre-delay to host BPM (8th note delay
  before the tail is a vocal classic)
- **Freeze** — infinite reverberation; modulate Cutoff to morph the
  frozen pad
- **Sidechain HPF** the SC input upstream (saturator with HPF SC)
  if you want kick to duck the reverb but not sub-bass

## DSP details

- Tap reads use 3rd-order Lagrange interpolation — sweep SIZE/PD
  smoothly without aliasing clicks
- DC blocker before the feedback loop kills drift in long tails
- LFO-modulated allpass coefficients break up the metallic ringing of
  pure Dattorro
