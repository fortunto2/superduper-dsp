# SuperDuper Morph

Real-time timbre morph. The main input (a voice, any monophonic line) keeps its
performance: pitch, timing, dynamics, consonants. Its timbre moves toward a
target as **Morph** goes 0 → 1. Spectral modelling, no neural net.

CLAP id `co.superduperai.morph`, AU `aufx sdMo`, latency **2048 samples**
(42.7 ms at 48 kHz, reported to the host). ~1 % of one Apple-silicon core.

## Targets (`Source`)

| Source | What it is |
|---|---|
| **Sidechain** | Anything on the second input port ("Target"). Analysed live like the main input: its own f0, harmonic levels *by harmonic number*, envelope, noise share. A target at any pitch lends its profile to the voice at the voice's pitch. REAPER: give the track 4 channels and route the target to 3/4 (the port falls back to main-port channels 3/4 the way Compressor's sidechain does). |
| **Captured** | **Capture** freezes the live sidechain timbre into the project (saved in the plugin state), so the target need not keep playing. |
| **Model** | The parametric Kubyz: 16 harmonics through its 1–6 formant bank. `Target` picks a factory preset; the mailbox can load any Kubyz patch. |

## Modes

- **Harmonic**: harmonic *k* of the voice goes to the level of the target's
  harmonic *k*, interpolated in dB (Morph 0.5 = halfway in dB on every
  harmonic). Noise between harmonics follows the target's envelope, scaled by
  **Breath**.
- **Envelope**: the voice's smoothed spectral envelope moves to the target's
  (in dB, on a 96-point log grid). Works on drums, noise, chords.
- Harmonic falls back to Envelope on any frame where either side is unpitched,
  so consonants and breath keep going.

## Params

Morph 0–1 (0.8) · Vowels 0–1 (0) · Breath 0–1 (0.3) · Source (Model) · Mode
(Harmonic) · Target (Bashkir Kubyz) · Tilt ±6 dB/oct · Mix · Output.
**Morph 0 is exactly the input delayed by the latency, bit for bit** (the wet
path crossfades into the delayed dry below Morph 0.05). Every frame is
loudness-matched, so Morph does not change the level.

**Vowels** swaps the target's envelope for the voice's: at 1 the target's
harmonic fine structure carries the voice's formants, so a kubyz "speaks" the
words.

## How it works

Per STFT frame (N 2048, hop 512, `synth_core::spectral::StftProcessor`): YIN f0
for each input, harmonic peaks (parabolic, up to 40, below 16 kHz),
harmonicity → voicing, an envelope bridging the harmonic comb
(`synth_core::timbre`). `timbre::morph_gains` returns one real gain per bin,
applied to L and R with their phases untouched. Gains are capped at +30 /
−60 dB: a harmonic the voice does not have can be raised, not created.

## Copy a running Kubyz (mailbox)

```text
h = plugin_get(track, "Kubyz", "harmonics")
plugin_set(track, "Morph", "target", h)      # model := that patch, Source := Model
plugin_set(track, "Morph", "target", {"capture": true})   # freeze the sidechain
plugin_get(track, "Morph", "target")          # model, captured, live f0 + harmonic bars
```

## Vocoder vs Morph

SuperDuper Vocoder's Spectral mode is cross-synthesis: the voice's smoothed
envelope on the carrier's fine structure, **at the carrier's pitch**. Its
"Voice → kubyz morph" recipe automates Mix, which is a crossfade between two
sounds. Morph is the continuous version of the same idea with the roles kept:
the voice's pitch and timing stay, only the timbre moves. At Morph 1 +
Vowels 1 Morph *is* that cross-synthesis (target source × voice envelope),
with the target's fine structure re-indexed by harmonic number onto the voice's
pitch instead of played at the carrier's.

## Tests

`tests/dsp_smoke.rs` (known answers, engine level): Morph 0 = delayed dry bit
for bit; self-morph −88 dB residual at the reported latency; Morph 1 lands on
the Bashkir profile at 220 Hz within 2 dB (measured ≤ 0.3 dB) and Morph 0.5
halfway (≤ 0.1 dB); a 330 Hz sidechain profile lands on a 220 Hz voice within
2 dB (≤ 0.3 dB), and the same through Capture → JSON → Captured; a 200 → 300 Hz
glide stays within 0.7 cents of the input's harmonics; white noise stays
unvoiced with no tone > 1.7 dB over its local floor; Envelope mode gives noise
the target's −22 dB 1 → 4 kHz slope (−21.4). `tests/quality.rs`: snapshot,
param table, no allocation in `process()`, Morph 0 through a real CLAP host.
`tests/targets.rs`: Kubyz tables pinned to the Kubyz plugin; state round-trip.

## Next: a neural upgrade (DDSP / RAVE)

The parametric target is the slot a model fills. A DDSP decoder trained on
recordings of the target (f0 + loudness → harmonic amplitudes + noise filter)
would replace `TargetFrame::from_model`: the same per-harmonic dB levels,
learned instead of 16 hand-set bars, and a noise filter instead of the
envelope. That needs a training set per timbre (10–30 min of the instrument),
an f0/loudness front end the plugin already has, and an inference path on the
audio thread (a small GRU, ~1 ms per frame; `synth_core::nam` shows the
pure-Rust pattern). The bigger change, RAVE, replaces the STFT gain stage with
a latent encoder/decoder and drops the "harmonic must exist in the voice"
limit, at the cost of ~100 ms+ latency and a GPU-trained model per target.
