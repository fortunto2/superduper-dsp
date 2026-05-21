# SuperDuper Sampler

Polyphonic WAV player with YIN pitch tuner, multi-mode SVF filter,
reverse playback, loop region, velocity mapping, and pack discovery
from your music folders.

| | |
|---|---|
| Category | Instrument (MIDI-in → audio-out) |
| Stereo | yes |
| Latency | 0 samples |

## Sample folders

Drop WAV files / packs into one of these (recursive scan, max depth 4):

- `~/Music/SuperDuper Samples/`
- `~/Music/Favorite 808s/`

Each sub-folder becomes a **Pack** in the dropdown. Add custom roots
via the in-app settings (persisted to disk).

### Free WAV sources

- **Goldbaby Free Stuff** — <https://www.goldbaby.co.nz/freestuff.html>
- **Hyperreal Music Machines — TR-808** — <http://machines.hyperreal.org/manufacturers/Roland/TR-808/>
- **Hyperreal — TR-909** — <http://machines.hyperreal.org/manufacturers/Roland/TR-909/>
- **Hyperreal Roland index** — <http://machines.hyperreal.org/manufacturers/Roland/>

## Pitch

YIN auto-detects the sample's root on import (or pick manually).
Playback rate = `2^((MIDI note - Root + Tune + Fine/100) / 12)`.

The waveform display shows the **detected pitch** + cents offset +
the played note after Tune/Fine. `→ Root` button snaps Root to the
detected pitch.

## Loop region

Drag the green/orange markers on the waveform to set Loop Start /
Loop End. **Loop** toggle wraps playback inside the region after the
initial attack. **Reverse** plays the entire sample backwards (loop
region still respected).

## Filter

5-mode SVF per voice:

| Mode | What |
|---|---|
| Off | Bypass |
| LP | Low-pass |
| HP | High-pass |
| BP | Band-pass |
| Notch | Band-reject |

Cutoff in Hz, Resonance up to self-oscillation, **Env→Cutoff**
modulates from the amp envelope (one-knob filter swell).

## Velocity

- **Vel→Amp** — scales note loudness with MIDI velocity (1 = full,
  0 = ignore)
- **Vel→Cutoff** — opens the filter on harder hits (natural drum
  dynamics)

## Parameters

| Group | Params |
|---|---|
| Pitch | Root, Tune, Fine |
| Loop | Loop, Loop Start, Loop End |
| Trim | Start, End, Reverse |
| Envelope | Attack, Decay, Sustain, Release |
| Filter | Filter (mode), Cutoff, Reso, Env→Cutoff |
| Velocity | Vel→Amp, Vel→Cutoff |
| Output | Output |
