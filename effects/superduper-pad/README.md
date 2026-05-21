# SuperDuper Pad

Polyphonic MIDI pad synth. 8 voices, TPT/ZDF SVF + tanh, click-free
voice steal, soft-fade choke, MIDI CC + pitch-bend.

| | |
|---|---|
| Category | Instrument (MIDI-in → audio-out) |
| Stereo | yes |
| Latency | 0 samples |

## Engine

Per voice: 4-partial oscillator + TPT/ZDF SVF lowpass + tanh
saturation + ADSR envelope. 8-voice pool with age-based stealing —
oldest voice (or first-released, if any) gets re-assigned on overflow.

Voice steal preserves SVF integrator state + oscillator phases so
there's **no click** on overflow. Envelope re-gates via `gate_on()`
which resumes from the current level (no zero-attack jump).

## Parameters

| Group | Params |
|---|---|
| Tone | Cutoff, Resonance, Drive |
| Envelope | Attack, Decay, Sustain, Release |
| Modulation | Modulation depth (CC 1 routing) |
| Pitch | Bend Range (semitones) |
| Output | Output |

## MIDI

| Source | Default routing |
|---|---|
| Pitch bend (0xE0) | ±Bend Range semitones |
| CC 1 (Modulation) | Modulation depth knob |
| CC 11 (Expression) | Drive |
| CC 71 (Filter Resonance) | Resonance |
| CC 74 (Filter Cutoff) | Cutoff (log) |

## Use cases

- Soft pad bed under a vocal
- Background drone on key changes
- Soundtrack atmosphere (combine with [SuperDuper Reverb](../superduper-reverb/README.md) on a big plate)
