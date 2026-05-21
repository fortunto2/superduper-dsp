# SuperDuper Ambient

Autonomous chord-drone generator — no MIDI input required. Drop on a
track and it plays a slow evolving pad on its own. Great for ambient
beds, soundtrack underlays, idle-state DAW background.

| | |
|---|---|
| Category | Instrument (no MIDI input, audio-out only) |
| Stereo | yes |
| Latency | 0 samples |

## What it does

4-voice `PadVoice` engine cycles through a slow chord progression
chosen at instance time (one of a handful of factory progressions —
Cm/Em/Gm/F minor, A minor pentatonic moods, etc.). Each voice has its
own LFO drift, slowly detuning around its centre note.

## Parameters

| Param | What |
|---|---|
| Cutoff | TPT/ZDF SVF lowpass on the mix |
| Resonance | Up to self-oscillation |
| Drive | Tanh saturation on the mix |
| Sustain | Master envelope sustain (defines drone tempo) |
| Output | Output gain |

## DSP

Built on `synth_core::dsp_blocks::PadVoice` — 4 partials per voice +
per-voice TPT/ZDF SVF lowpass + tanh saturation. Voices never gate
off; they drift forever.

## Use cases

- DAW background while composing
- Soundtrack pad bed (record one take, layer real instruments on top)
- Generative ambient — leave running for an hour and bounce
- Sleep audio
