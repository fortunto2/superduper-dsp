---
name: sdsp-mix
description: Mix and master a track with the SuperDuper plugins the way a professional engineer would — kick/bass sidechain ducking, a pocket for the vocal, mono low end, parallel drum compression, and a mastering chain aimed at a loudness target that keeps its dynamics. Includes tools/mixcheck.py for measuring a mix against a commercial reference. Use when a render sounds "flat", "muddy", "doesn't hit", "too quiet next to other tracks", or before delivering anything. Do NOT use for writing the arrangement (superduper-song) or building plugins (superduper-plugin).
---

# sdsp-mix — mixing and mastering that is measured, not guessed

Every mix fault on this project was invisible until measured, and obvious after.
A kit rendered with no hi-hats at all. A master whose three drops sat within
0.1 dB of each other. A build louder than the drop it was building to. Weeks of
listening missed all three; four seconds of `mixcheck.py` found each one.

So the loop is: **render → measure → compare to a reference → change one thing**.
Not "listen and tweak".

## Measure first

```bash
python3 tools/mixcheck.py mix.wav --ref ~/Music/!electro/some-commercial-track.wav
python3 tools/mixcheck.py mix.wav --bpm 140 --bars 8,16,32,40,56,64,76,80
```

What the numbers mean:

| Reading | Healthy | What a bad value means |
|---|---|---|
| crest factor | 10–13 dB | under 8: the chain flattened the performance; over 14 with a quiet mix: nothing is gluing it |
| band delta vs reference | within ±3 dB | ≥4 dB reads as "darker" / "harsher" than the reference, every time |
| correlation | +0.4 … +1.0 | below +0.2 the sides cancel on a phone and in a club |
| section arc | build < drop | a build louder than its drop means the drop never lands, whatever the sounds are |

Absolute band targets are a myth — genres differ by 10 dB. **Always pass `--ref`**
with a commercial track in the same style. `~/Music/!electro/` has plenty.

## The moves, in the order they matter

### 1. Sidechain the bass off the kick — do this before anything else

Kick and bass both live in 40–120 Hz. Summed, they stack into a peak the limiter
then has to eat, which is exactly how a mix ends up loud and gutless. Duck the
bass 4–6 dB for ~90 ms on every kick and it hands back the headroom.

Headless, one line in the chain config:

```toml
[[track]]
name = "kick"
input = "kick.wav"
mute = true              # optional: key only, never heard

[[track]]
name = "bass"
input = "bass.wav"
duck_from = "kick"       # appends a compressor keyed off the kick's render
duck_db = 5.0            # depth in dB
duck_release_ms = 90     # back up before the next beat (90 ms suits 100-150 BPM)
```

Any stage can key off any earlier track with `sidechain = "track:<name>"` —
`duck_from` is just the sugar for the common case.

In REAPER, the routing is: target track to 4 channels, a send from the drums to
its channels 3/4, compressor last in the chain, plugin input pins 2/3 mapped to
channels 3/4. `demos7/apply_fx.py::setup_sidechain` does all of it over the
bridge and is safe to re-run — copy that function rather than clicking.

Compressor settings for ducking (not for compressing):
`Threshold -26, Ratio 6, Attack 0.5 ms, Release 90 ms, Knee 3, SC HPF **0**,
Lookahead **0**, Auto Rel off`. Two of those matter more than the rest: **SC HPF
off**, because the key *is* low end and filtering it deafens the detector, and
**Lookahead 0**, because ducking before the hit sounds like a mistake.

Verify it, don't assume: measure the bass 5 ms after a kick and 150 ms after. A
working duck shows ~6–10 dB between them.

### 2. Clear the low end everywhere else

High-pass everything that is not kick or bass at 80–120 Hz — pads, vocals,
percussion, reverb returns. Rumble you cannot hear still eats the limiter.
Kick itself gets a 30 Hz high-pass; nothing musical lives below that.

### 3. Mono under 120 Hz

Wide bass smears on club systems and cancels in mono. `midside` plugin, or keep
sub sources mono by construction. `mixcheck` prints side/mid — under 120 Hz it
should be strongly negative.

### 4. Cut a pocket for the voice

If a vocal or a rap is coming, take 3–5 dB out of 300–800 Hz on the pads and
anything else harmonic, with a wide Q. Do it **before** the voice arrives, so
the beat is already right when it does. In a finished instrumental this reads as
a dip in `body` vs the reference — that is correct, not a fault.

### 5. Parallel compression on drums, not serial

`compressor` with `Mix 0.3–0.5`, `Ratio 4:1`, attack 5–10 ms. Keeps the
transients while raising the body. Serial compression at the same ratio just
removes the punch.

### 6. Reverb that doesn't fog the mix

Pre-delay 20–40 ms (the dry hit lands first), high-pass the return at 300 Hz,
and use the built-in `Duck Amount` so the tail steps out of the way while the
source sounds. Our reverb, supermass, delay and formant all have ducking built
in — they take the dry signal as the key when no sidechain is routed.

### 7. Resonances: soothe, don't EQ

A static EQ cut that fixes the loud note ruins every other note. `soothe` cuts
only when the band actually spikes. Amount 6–9 dB, Sens −9, bracket the range
you care about (2.5–9 kHz for harsh vocals, 200–500 Hz for boxy).

## The mastering chain

Order matters, and each stage should do one job:

```toml
[[master]]  # 1. tone — fix the balance the reference told you about
plugin = "eq"
params = { HP = 30.0, "Mid Freq" = 350.0, "Mid Gain" = -2.0, "Mid Q" = 0.9 }

[[master]]  # 2. lift, if mixcheck says the top is dark
plugin = "eq"
params = { "Mid Freq" = 1600.0, "Mid Gain" = 5.0, "Mid Q" = 0.4, "High Freq" = 8000.0, "High Gain" = 6.0 }

[[master]]  # 3. glue — gentle, parallel, slow attack
plugin = "compressor"
params = { Threshold = -14.0, Ratio = 2.2, Attack = 15.0, Release = 120.0, Mix = 0.45, Makeup = 1.0 }

[[master]]  # 4. harmonics, not distortion
plugin = "saturator"
params = { Drive = 4.0, Type = 0.0, OS = 2.0, Mix = 0.3, Output = -1.0 }

[[master]]  # 5. ceiling only
plugin = "limiter"
params = { Input = 0.0, Ceiling = -1.0, Release = 60.0, "True Peak" = 1.0 }
```

**Aim for −9 to −11 LUFS, not −7.** Past that point every extra dB comes
straight out of the crest factor: measured on this project, pushing to −7.9 LUFS
flattened three drops to within 0.1 dB of each other and undid the arrangement.
Streaming normalises to −14 anyway, so the loud version is louder nowhere and
flatter everywhere. Keep `True Peak = 1` and the ceiling at −1 dBTP so lossy
encoders have room.

The compressor's attack has to stay **above** the kick's transient (12–15 ms) on
a master, or the chain eats the punch it is supposed to glue.

## Symptom → measurement → fix

| It sounds… | Measure | Usually |
|---|---|---|
| dull, muffled | band delta in presence/air | synth hats and voices have no top of their own — add a bright source, don't just EQ air into nothing |
| muddy, boxy | body vs reference, correlation | no high-passes; two parts sharing 200–400 Hz |
| doesn't hit | crest, section arc | limiter over-driven; or the build isn't below the drop |
| bass disappears on small speakers | band balance, sub vs kick | all energy under 60 Hz; saturate the bass for harmonics |
| kick and bass fight | peak vs rms in the low band | no sidechain — see move 1 |
| flat, no journey | section arc | every section at one level; automate arrangement density, not just gain |
| harsh | presence delta, soothe meter | resonance, not overall brightness |

## Rules of thumb worth keeping

- Change **one** thing, re-measure. Two changes and you learn nothing.
- Fix it in the source before the master. A master EQ boosting 8 dB of air is a
  sign the arrangement has no air in it.
- The reference is not a target to match exactly — it is a sanity check for how
  far off you are.
- Loudness last, and lower than you think.
