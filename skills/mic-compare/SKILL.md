---
name: mic-compare
description: Compare two microphones (or two takes of one mic) with numbers instead of impressions — frequency response, noise floor, per-band SNR, plosive count, sensitivity — and decide what EQ can fix versus what is baked in. Use when the user says "какой микро лучше", "сравни микрофоны", "оцени запись", "стоит ли возвращать микрофон", "почему звучит глухо/ярко", "шумит ли микрофон", or drops two vocal takes expecting a verdict. Also covers making a level-matched A/B and counting plosives. Do NOT use for mixing a take (sdsp-mix) or analysing a full song (track-teardown).
---

# mic-compare — judge microphones by measurement

An agent cannot listen, but every claim people make about mics — "brighter",
"cleaner", "muddy", "picks up the room" — is a number. Measure it, and half
of the audible differences turn out to be recording mistakes, not the mic.

## Protocol: the recording decides whether the comparison is valid

1. **Same preamp gain for both takes.** This is the one that flips verdicts.
   Measured 2026-09-19: with free-run takes the AT2040 looked 5.7 dB cleaner
   than an SM58 replica; with matched gain the replica was 2.8 dB cleaner and
   4 dB more sensitive. Everything downstream of a gain mismatch is fiction.
2. Same phrase, same distance (a fist from the grille), 30+ seconds, with
   real pauses — the pauses are where the noise floor is measured.
3. Peaks at −12…−6 dBFS. A take whose peak prints 0.00 dBFS is clipped;
   count samples at full scale before trusting anything else about it.
4. One take per mic minutes apart is fine; the analysis is statistical.

## Metrics, in the order they decide things

Run everything with `uv run --with numpy --with scipy --with soundfile
--with pyloudnorm python …`. Frames of 50 ms; "voiced" = frames above the
70th RMS percentile, "silence" = below the 10th.

| metric | how | what it answers |
|---|---|---|
| LUFS / peak / clipped-sample count | pyloudnorm + `abs(x)>=0.9999` | is the take usable; sensitivity (at matched gain the LUFS gap IS the sensitivity gap) |
| SNR | RMS p95 − p5 | overall cleanliness |
| spectrum shape | mean voiced PSD, each mic **normalized to its own 300–3000 Hz energy** | frequency response without caring about level |
| per-band voice−noise | voiced PSD minus silence PSD, per band | **whether a "bright top" is signal or hiss** — the decisive one |
| noise bands | silence PSD relative to own voice ref: 20–80 (rumble), 80–300 / 300–2k (room), 2–8k / 8–16k (hiss) | what kind of noise, and whose |
| hum | nearest-bin peaks at 50/100/150/200 Hz in silence | mains pickup |
| plosives | see below | the pop-filter question, in numbers |

Macro bands that map to words: sub <80 · body 80–200 · mud 200–500 ·
mid 500–2k · presence 2–5k · sibilance 5–9k · air 9–16k.

## Plosive counter

Band-pass 20–110 Hz and 300–3000 Hz, 25 ms frames. An event = LF exceeds its
own typical in-voice level by >10 dB AND exceeds the mid band (an air blast,
not a loud syllable). Group adjacent frames, report events/min and the worst
spike. Reference points from a real session: AT2040 (built-in screen)
16.2/min worst +18.3 dB; bare SM58 replica 31.7/min worst +22.2 dB — the
integrated pop filter is worth a 2× reduction, which is a purchase reason
you can print.

## The verdict rubric

- **Frequency response differences are cheap.** A shelf fixes a dark top in
  one knob. Quote the dB and move on.
- **Per-band SNR differences are forever.** An EQ boost raises signal and
  noise by exactly the same amount — measured: +9 dB shelf left the 8–16 kHz
  SNR at 23.8 dB, untouched. If mic A has 6 dB worse SNR where it needs
  boosting, mic B wins even if A measures flatter.
- **Room pickup (noise at 300–2000 Hz) cannot be EQed out** — it lives under
  the voice. Same for handling noise and hum.
- A noisy top that masks under a beat matters less than a plosive that cuts
  through any limiter. Weigh noise by where it sits in the final mix.
- Say which claims are *measured here* vs *reported* (spec sheets lie about
  replicas; a replica SM58 measured 4 dB hotter and +8 dB at 8–12 kHz vs
  the original's published curve).

## Level-matched A/B for human ears

Normalize both takes to −20 LUFS (`pyloudnorm`, then scale), write 24-bit
WAVs plus one concatenated file with 0.7 s gaps. If testing "can EQ close
the gap", render a third variant through `sdsp-chain` (eq stage, High
Freq/High Gain) and REPORT the per-band SNR of all three — the shelf closes
the tone gap while the SNR table shows its price. Working example:
`~/Music/1music/demos6/mic_ab/` (A_2040 / B_58a / C_2040_shelf / ABC_joined).

## Traps, all measured 2026-09-19

- **Gain mismatch flips verdicts** (see protocol #1). Never compare takes
  recorded with different knob positions except for response *shape*.
- **peak 0.00 dBFS** means clipped, not "hot": count full-scale samples.
- A brighter top is only proven hiss if the band's voice−noise SNR is worse;
  ours was equal, so the brightness was real signal (retracting the first
  claim cost a message — check SNR before calling it noise).
- **Never A/B via two renders of non-deterministic synths** — free-running
  LFOs and noise oscillators made two identical renders differ by 9.8 dB
  frame-to-frame, burying a 3 dB effect. Compare the takes themselves, or
  make the control path share every latency (e.g. ratio 1:1, not bypass).
- Condenser death by humidity/salt air (crackle, random noise bursts) is a
  polarized-capsule failure: try a week in a sealed box of silica gel, never
  heat. Dynamics are immune — for sea air and travel, that decides the type.
