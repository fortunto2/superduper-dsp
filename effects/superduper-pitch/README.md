# SuperDuper Pitch

A pitch + **independent formant** shifter with **two engines** (`Mode`):

- **Voice** — TD-PSOLA, best on a solo monophonic voice, with fully independent
  formant ("manual auto-tune"): shift up (Masyanya / chipmunk) or down (bass /
  demon), and move the **formants separately** so you can raise pitch *without*
  the chipmunk effect, or change body/gender *without* changing the note.
- **Track** — a phase vocoder that transposes **polyphonic** material: whole
  mixes, chords, drums, a full song. Use it to **change the key of a track**.

Pick Voice for a single voice; pick Track to transpose anything with more than
one note at a time.

## How Voice mode works — TD-PSOLA

1. A streaming **YIN** tracker estimates the voice's local pitch period `T0`
   (on the L+R mono sum).
2. **Pitch** shift by `α = 2^(Pitch/12)`: pitch-synchronous grains (one period,
   Hann) are re-spaced at `T0/α`. Denser marks (α>1) raise the pitch; sparser
   marks (α<1) lower it. One-period grains are used on purpose — a 2-period
   grain reads contiguous input and the overlap-add just reconstructs the
   source on a downshift; a 1-period grain leaves real gaps so the pitch
   actually drops.
3. **Formant** shift by `β = 2^(Formant/12)`, applied by reading each grain's
   samples at a `β`-scaled step from the input. This stretches the grain's
   spectral envelope **without** touching the synthesis mark spacing — so pitch
   and formant are **independent**:
   - `Pitch +12, Formant 0` → an octave up, *same timbre* (not a chipmunk).
   - `Pitch 0, Formant +5` → same melody, smaller/brighter throat (gender flip).
4. Grains are weighted-overlap-added and blended with the latency-matched dry
   signal (`Mix`).

Tuned for **monophonic voice** (or a solo instrument) — it tracks the pitch of
what you feed it, so clean single-note input works best.

## How Track mode works — phase vocoder

An STFT phase vocoder (smbPitchShift-style, 2048-point FFT, 75 % overlap). Each
window is analysed into per-bin magnitude + *true* frequency (from the phase
advance across hops); the bins are moved to `bin × 2^(Pitch/12)` positions, the
synthesis phase is re-accumulated, and an inverse FFT + overlap-add rebuilds the
signal. Because every spectral component is re-pitched independently, it
transposes **any** input — a chord, a drum loop, a full mix — which is exactly
what "change the key of a track" needs. `Formant` optionally warps the spectral
envelope (less meaningful on a full mix; default 0 = leave it).

## Latency

Both engines report the **same** fixed latency (~43 ms at 48 kHz) via the CLAP
`latency` extension, so switching Mode never disturbs the host's delay
compensation. Mix through it freely; for the lowest-latency live monitoring keep
shifts modest.

## Parameters

| Param | Range | Notes |
|---|---|---|
| Pitch | −24…+24 st | Pitch shift (works in both modes) |
| Formant | −12…+12 st | Formant/timbre shift, independent of pitch (Voice) |
| Mix | 0–1 | Dry/Wet |
| Output | −24…+24 dB | Output trim |
| Mode | Voice / Track | PSOLA (solo voice) or phase vocoder (polyphony) |
| Target | None / C…B maj/min | Target key for the **Match** button |

## Key detection & Match

The plugin analyses the incoming audio and shows its **detected musical key**
live in the GUI (Krumhansl-Schmuckler — a 12-bin chromagram correlated against
the 24 major/minor key profiles), e.g. `key in: A minor (82%)`.

To **match keys between two tracks**: put an instance on the reference track
(say the vocal) and read its key; put another on the track you want to move
(say the music), pick that key as **Target**, and hit **Match** — it sets
`Pitch` to the nearest-octave interval that transposes the music into the
vocal's key. (Use Track mode for full mixes.)

## Presets

| Preset | Mode | Pitch | Formant | For |
|---|---|---|---|---|
| Chipmunk | Voice | +12 | 0 | Octave up, formants preserved |
| Masyanya | Voice | +8 | +4 | Nasal cartoon (pitch + formant up) |
| Bass | Voice | −12 | 0 | Octave down, natural timbre |
| Demon | Voice | −8 | −5 | Menacing monster (down + throat lowered) |
| Gender Flip | Voice | 0 | +5 | Change the body, keep the melody |
| Deeper | Voice | −4 | −2 | Subtle deepening |
| Key +2 | Track | +2 | 0 | Transpose a whole track up a tone |
| Key −2 | Track | −2 | 0 | ...down a tone |
| Key +5 | Track | +5 | 0 | Up a fourth |
| Key −5 | Track | −5 | 0 | Down a fourth |

## Verified

Objective tests: **Voice** (`tests/pitch_accuracy.rs`) — `Pitch +12` → f0 ×2,
`Pitch −12` → ×0.5, `Pitch 0` transparent, `Formant +7` at `Pitch 0` raises the
spectral centroid while the fundamental stays put. **Track**
(`tests/polyphony.rs`) — a C-E-G triad through `Pitch +2` moves **all three**
tones up by 2^(2/12) with the originals gone (polyphonic transposition).
