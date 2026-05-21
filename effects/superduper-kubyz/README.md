# SuperDuper Kubyz

Physical-model jaw harp / khomus. 16-harmonic additive engine shaped
by a 3-band formant filter, driven by an interactive IPA vowel pad
with animated mouth trajectory.

| | |
|---|---|
| Category | Instrument (MIDI-in → audio-out) |
| Stereo | yes (mouth trajectory pans) |
| Latency | 0 samples |

## Engine

- **Additive oscillator** — 16 user-editable harmonic amplitudes
- **3-band bandpass formant filter** — F1 / F2 / F3 resonances
- **Vowel pad** — drag the cursor on the IPA grid to morph between
  Peterson-Barney male-average formants of ɑ, ɛ, i, o, u
- **Auto-mouth motion** — Circle / Sine / Figure-8 / Triangle / Line
  trajectories through formant space, tempo-syncable
- **Stereo motion** — mouth trajectory pans L/R for a wider field

## Mouth pad (IPA vowels)

The vowel grid shows F1 (jaw aperture) on the Y-axis and F2 (tongue
position) on the X-axis. Drag the cursor to morph between resonance
configurations. Snap targets near each IPA vowel:

| Vowel | F1 | F2 | F3 |
|---|---|---|---|
| ɑ (father) | 730 | 1090 | 2440 |
| ɛ (bed) | 530 | 1840 | 2480 |
| i (see) | 270 | 2290 | 3010 |
| o (boat) | 570 | 840 | 2410 |
| u (boot) | 300 | 870 | 2240 |

Different vowels = different timbres on the same fundamental note.

## Mouth motion

- **Mouth Shape** — Circle / Sine / Figure-8 / Triangle / Line
- **Mouth Rate** — Hz when M Sync off, tempo division when on
- **Mouth Depth** — how much the trajectory moves through formant space
- **Mouth Stereo** — pans the motion left/right
- **M Sync** — lock Mouth Rate to host BPM

## Presets

- **Bashkir** — folk khomus tuning, breathy
- **Khomus** — Yakutian metal jaw harp, hard timbre
- **Real-D2** — analysed from a real recording in D2 (companion
  `tools/kubyz_analyser` fits your own)

## Parameters

| Group | Params |
|---|---|
| Voice | Drive, VoxMix, Sustain |
| Mouth | Mouth Depth, Mouth Stereo, Mouth Shape, Mouth Rate, Mouth Div, M Sync |
| Formant | F1, F2, F3 (interactive via pad) |
| Envelope | Attack, Decay, Sustain, Release |
| Output | Output |

## MIDI mapping (defaults)

| CC | Param |
|---|---|
| CC 1 | Mouth Depth |
| CC 2 | Mouth Stereo |
| CC 11 | F1 |
| CC 71 | F3 |
| CC 74 | F2 |

Send Modulation + Expression from a foot controller for hands-free
morphing while playing.

## Workflow

1. Pick a preset (Bashkir / Khomus / Real-D2).
2. Tweak with the Mouth pad first — drag to find the timbre you want.
3. Adjust formant gain second to balance overtones.
4. Shape the envelope last.
