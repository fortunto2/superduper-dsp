# SuperDuper Drum

6 analog-synthesis drum voices on consecutive white keys C-D-E-F-G-A.
Kick / Snare / HH closed / HH open / Clap / Cowbell. On-screen
mini-keyboard, mouse-click pads, MIDI passthrough.

| | |
|---|---|
| Category | Instrument (MIDI-in → audio-out) |
| Stereo | yes |
| Latency | 0 samples |

## Voice map

| MIDI Note | Voice | Synthesis |
|---|---|---|
| C3 | Kick | Sine + pitch envelope + click |
| D3 | Snare | Noise band-pass + tone oscillator |
| E3 | HH closed | Square ring-mod + HP |
| F3 | HH open | HH closed + longer decay |
| G3 | Clap | Multi-tap noise burst |
| A3 | Cowbell | Square ring-mod + BP |

## Parameters per voice

| Param | What |
|---|---|
| Level | Voice output gain |
| Tune | Pitch offset (kick boom, snare body, …) |
| Decay | Tail length |

Plus master:

| Param | What |
|---|---|
| Drive | Tanh saturation on the mix |
| Master | Output gain |
| Note Out | Pass MIDI through to downstream synths (Wave/Kubyz can play in unison) |

## Workflow

- MIDI clips trigger the 6 pads via standard notes
- `Note Out` lets a single MIDI clip drive both Drum and a melodic
  layer (bass synth on Wave / monophonic lead on Kubyz)
- Mouse-click pads for live performance / programming without a
  controller
