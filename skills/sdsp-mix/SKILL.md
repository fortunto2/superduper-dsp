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

Compressor settings for ducking (not for compressing) — **depth comes from
`Range`, not from threshold arithmetic**:
`Threshold -30, Ratio 10, Range = the depth you want (4-6 dB), Attack 0.5 ms,
Release 80 ms, Knee 3, SC HPF **0**, Lookahead **0**, Auto Rel off`. The
threshold sits far below any hit, the steep ratio slams into the Range cap, so
the duck is exactly Range dB regardless of how hot the key is. (Setting depth
via threshold-overshoot × ratio — Thr −10 / Ratio 1.6 hoping for ~4 dB —
measured 1.3 dB on real material.) SC HPF stays off because the key *is* low
end; Lookahead 0 because ducking before the hit sounds like a mistake.

Match **Release to the gap after the key**, not to taste alone: GR holds while
the key sounds and needs Release + detector time (~150 ms total) to come back,
so a 300 ms kick tail in a 580 ms beat leaves the bass only ~150 ms of freedom.
A long swelling key means a duck that never breathes — shorten the kick, don't
fight the release.

Verify it, don't assume — and not by comparing the bass to itself (when kick
and bass strike the same beat, the bass's own attack confounds "5 ms vs 150 ms
after the hit"). Render the ducked track twice, with and without the duck
stage, and plot `20*log10(env_ducked/env_dry)` against the kick envelope: the
curve must sit at −depth while the kick sounds and return to 0 before the next
beat. A flat line at −depth is a fader, not a duck.

And when the A/B says **0.00 dB difference**, check the DRY state before
blaming the routing (measured 2026-09-23): with the music loud enough that its
own dry-fallback GR also slams into the Range cap, keyed and un-keyed states
are BOTH exactly −Range — identical by construction, and the duck is invisible
to any off/on comparison. For the verification render set Range 0 (or drop the
music below threshold); the same rig then showed a 9.4 dB duck in an offline
render. The plugin and REAPER's ch3/4 routing were never the problem.

**Fixed 2026-08-27, worth remembering:** the keyed plugins used to decide
"sidechain routed?" per 256-frame block by checking for non-zero samples — so
between kick hits the silent key handed detection back to the MAIN input, and
with a low threshold a compressor's GR never released (the duck measured as a
constant −Range) while a ducker re-keyed off dry. All five (compressor,
delay, reverb, supermass, vocal) now share a latch
(`sdk::clap_helpers::SidechainSnapshot`): once a key is seen, silence on it
means release. Renders made before the fix have static gain where a pump was
intended.

### 2. Clear the low end everywhere else

High-pass everything that is not kick or bass at 80–120 Hz — pads, vocals,
percussion, reverb returns. Rumble you cannot hear still eats the limiter.
Kick itself gets a 30 Hz high-pass; nothing musical lives below that.

### 3. Mono under 120 Hz

One insert now: SuperDuper Mid/Side, `Mono Below = 120` (a 2nd-order HP on
the side signal; 0 = off). Known-answer on a pure-side test tone: 60 Hz side
down 12.3 dB with the crossover at 120, 1 kHz side untouched to 0.01 dB,
nothing leaks into the mid. Put it on the master right after the tone EQ —
it is the direct answer to "side energy below 120 Hz" reading hot in a mix
measurement.

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

## The channel chain — one order, only the numbers change

A channel strip's stage ORDER is the part that costs nothing and matters most.
The canonical vocal chain, each slot one job:

```
0 tune → 1 HP + trim → 2 mud cut → 3 comp FAST (peaks) → 4 comp SLOW (level)
       → 5 de-ess → 6 saturate → 7 tonal EQ → 8 delay + reverb
```

Why each thing sits where it sits — these are the rules, the numbers are taste:

- **Pitch correction first, on the dry take** (0). The tracker reads pitch
  reliably before anything nonlinear touches the signal.
- **Gain-stage into the chain, ride before the comps** (1). Aim ~−20 LUFS
  into stage 3 via the first eq's `Output`, and level uneven phrases there
  with `automate = { Output = … }` — that is clip gain, so the compressors
  work less. `gain_automate` on the track is post-chain: a fader, not a ride.
- **Subtractive EQ before compression** (1–2). The comps must react to the
  voice, not to rumble and box they then drag along.
- **Fast compressor before slow** (3→4) — the 1176-into-LA-2A stack. The
  fast FET-style stage (Pump curve) takes 3–5 dB off peaks only; the slow
  leveler (Smooth curve) then holds 2–3 dB constantly. Reversed, the slow
  one slams on every transient and pumps. The signatures are measurable in
  the per-stage printout: stage 3 should barely move the LUFS (peaks only),
  stage 4 should shave a steady 2–3 dB. 3 dB twice beats 6 dB once.
- **De-ess after the comps and before the saturator** (5). The comps just
  raised the esses; drive would then exaggerate them. Between the two is the
  only slot where a de-esser wins.
- **Saturation after compression** (6) — the drive amount stops depending on
  how loud the performer got. `Mix < 1` = parallel: expensive, not distorted.
- **Additive EQ after saturation** (7). It shapes the harmonics the drive just
  added; boosting *before* the drive feeds back the mud slot 2 removed.
- **Space last** (8), so it hears the finished, de-essed voice — a reverb fed
  before the de-esser sprays sibilance across every tail. Duck it. In a mix
  the pro topology is a 100%-wet return track; serial low-Mix is the shortcut.

Genre changes only the numbers (rap: hard tune, harder comp, slap not tail,
parallel comp Ratio 10 / Mix 0.35 for thickness; pop: more air and plate;
house: darker, longer). Ready starting config with per-genre notes:
`sdsp-chain --template vocal`. The same chain exists as a REAPER project
template ("SuperDuper Vocal Chain", File → Project templates) with the space
stages as ducked send returns and every parameter saved in the .rpp; its
generator lives in `~/Music/1music/vocal-template/` and reads the same TOML.

The same slot logic — clean → level → character → tone → control → space —
applies to any source chain, and the mastering chain below follows it too.

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

Since 2026-09-19 the limiter actually holds its ceiling: a smoothed-gain
engine keeps true peak within 0.1 dB of the setting (it used to overshoot by
up to 1.6 dB). Set `Ceiling` to the number you want on the meter — no more
padding it 1.5 dB low — and expect ~70 samples more latency, which the host
compensates. **Anything mastered before that date may read hotter than its
ceiling: re-measure old renders before re-releasing them.** Two more master
rules paid for in blood: the **master fader stays at 0** (a pulled-down
fader eats the limiter's ceiling downstream and looks exactly like a broken
limiter), and never judge a change by re-rendering non-deterministic synths
— free-running LFOs made two identical renders differ by 9.8 dB per frame.

**Aim for −9 to −11 LUFS, not −7.** Past that point every extra dB comes
straight out of the crest factor: measured on this project, pushing to −7.9 LUFS
flattened three drops to within 0.1 dB of each other and undid the arrangement.
Streaming normalises to −14 anyway, so the loud version is louder nowhere and
flatter everywhere. Keep `True Peak = 1` and the ceiling at −1 dBTP so lossy
encoders have room.

The compressor's attack has to stay **above** the kick's transient (12–15 ms) on
a master, or the chain eats the punch it is supposed to glue.

**Glue is 1.5–3 dB of GR, and that is a number to read, not guess.** A real
master ran at −7.6 dB GR (mix RMS −10 into a −24 threshold at 2:1 — the math
says 7, the meter agreed) and that surplus was both the audible pumping under
the kick and a squashed crest. Set the threshold so the meter shows 1.5–3,
put **Range 2–3 dB on the master glue** as a hard cap (the knob exists now),
and raise SC HPF to 120–300 if an 808 tail is what drives the pumping.

**Chain stages trade work — rebalance in pairs.** Relaxing that glue by 5 dB
sent 5 dB more into the limiter, which promptly ate the crest the relaxation
was meant to restore (9.6 → 8.7). Every time one stage backs off N dB, take
~N dB out of the limiter Input, then re-measure; the second render, not the
first, is the verdict.

**The limiter meters its own output now** (S/I LUFS + true peak, red past
−1 dBTP). Press reset, play the whole track once, and the I readout IS the
mastering verdict — external meters are for cross-checking, not for every
iteration. Two measurement traps: a render started while a parameter is
still landing measures a hybrid of both values (wait a beat after setting,
then render — one "ceiling broken?" scare was exactly this), and a +2.5 dB
master shelf at 12 kHz moved the 6–16 kHz band by 0.1 dB — air lives in
sources (hats, sparkle layers), the master EQ cannot invent it.

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
