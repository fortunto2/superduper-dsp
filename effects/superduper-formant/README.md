# SuperDuper Formant

A **mouth** for any sound. Three band-pass vocal-tract resonances — F1, F2, F3 —
imposed on whatever goes in, articulated by hand, by a voice, or by itself.

## Why it exists

A kubyz (Bashkir jaw harp) and a singing voice work the same way: a source rich
in overtones — reed or vocal folds — and a mouth cavity that decides *which*
overtones you hear. That cavity is three resonances. So the two instruments are
not two different sounds to crossfade between; they are one formant line with
two different excitations underneath.

This plugin is that idea made usable: **sing a phrase into the sidechain and a
kubyz drone speaks it**, and when you stop singing the last vowel stays frozen on
the drone. The voice hands the phrase over to the instrument instead of cutting
out. Started singing, ended as kubyz.

## Not a vocoder

`SuperDuper Vocoder` copies a modulator's entire spectral envelope onto a
carrier: intelligible, robotic, and it needs the voice sounding at every instant.
This models the mechanism instead — three resonances that glide. Consequences:

- It articulates with **nobody singing** (pad, trajectory, automation, gestures).
- It sings rather than talks: three glides, not a quantised spectrum.
- It survives a hand-off, because the tracked vowel can be **held**.

Use the vocoder for words. Use this for singing, and for making an instrument
speak.

## Modes

| Mode | What drives the formants |
|---|---|
| **Manual** | The IPA vowel pad — F2 across, F1 down (closed vowels at the top). Drag it, automate F1/F2, or drive them from gesture CCs. |
| **Follow** | A formant tracker reads F1/F2/F3 out of the voice on the `Voice` sidechain input (port 1) and imposes them on the main input. `Follow` blends between the pad and the tracked vowel. |
| **Motion** | A trajectory (Circle / Sine / Figure-8 / Triangle / Line) walks the pad on its own — free-running or locked to the host grid (`Sync` + `Div`). `Stereo` runs L and R anti-phase for width. |

## Parameters

`F1` `F2` `F3` — the three resonance centres.
`Width` — bandwidth scale. 1.0 = natural vowel Q (Peterson-Barney ratios); below
1 narrow and nasal with more ring, above 1 broad and airy.
`Shift` — transposes the whole formant set (±12 st) — bigger or smaller head.
`Mode` `Follow` `Glide` — articulation source, tracking depth, and glide speed.
`Path` `Rate` `Sync` `Div` `Depth` `Stereo` — the Motion trajectory.
`Drive` — saturates **before** the resonators. Matters more than it looks: a pure
sine has nothing for a formant to pick out, so add Drive when the input is too
clean to articulate.
`Mix` `Output` — wet/dry and trim. `Preset` — stepped selector, so a host or an
MCP agent can recall a vowel without opening the GUI.

## MIDI CC

Matches the live2play gesture defaults, so the phone articulates the formants:

| CC | → |
|---|---|
| 1 (mod wheel) | F1 — jaw open / close |
| 74 (brightness) | F2 — front / back vowel (hands apart) |
| 71 (resonance) | Width |
| 73 | Drive |
| 76 | Depth (motion amount) |

CC moves deliberately do **not** raise the automation-dirty flag, so they stay in
the MIDI clip instead of feeding back into the FX envelope.

## Routing the voice (REAPER)

1. Right-click the plugin in the FX chain → **Pin Connector**.
2. Enable 4-channel routing on the track (right-click track → I/O → 4 channels).
3. Connect track channels 3-4 → plugin pins 3-4 (the `Voice` L/R).
4. Send the vocal track to channels 3-4 of this track.

With nothing routed the tracker simply sees silence and holds — Manual and
Motion work regardless.

## Tuning note

The kubyz reed is a fixed pitch and it is **not** in equal temperament (the drone
sits near C#2 / A#2+29c / D2). Match the singing to the drone rather than to
12-TET, or the articulation rides a beating fundamental.

## Tests

```bash
cargo test --release -p superduper-formant
cargo test --release -p superduper-formant --test spectrum -- --nocapture
```

`dsp_smoke` asserts the resonators stand ≥10 dB above the inter-formant valley,
that Follow copies a sung /i/ onto a 100 Hz drone, and that the vowel holds after
the voice stops. `spectrum` prints ASCII spectra — the Follow chart should look
like the sung vowel drawn on the drone's harmonics.
