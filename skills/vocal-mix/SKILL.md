---
name: vocal-mix
description: Mix Rustam's recorded vocal in REAPER on the SuperDuper + UADx hybrid chain — the proven slot-by-slot chain for his voice and AT2040 (plosive repair, gate before compression, 1176→LA-2A, 610-B air, de-ess, ducked delay/reverb returns, beat ducked under the voice via the Voice Duck preset) with the exact numbers that shipped. Use when the user says "сведи вокал", "настрой цепочку вокала", "голос глухо / шумит / плюётся / тонет в музыке", "сделай как в newlife", or drops a fresh vocal take into a REAPER project. Do NOT use for whole-mix balance and mastering theory (sdsp-mix), headless renders (sdsp-chain), or judging microphones (mic-compare).
---

# vocal-mix — Rustam's vocal, slot by slot

Everything below shipped on a real track (newlife, 2026-09-19) and was
verified by render measurement, not by ear alone. The order is the
instrument (see sdsp-mix "The channel chain" for why); the numbers are the
starting point for THIS voice on THIS mic.

## Recording constants (before any plugin)

- **AT2040**, Arrow preamp **47–48 dB**, LOW CUT on the Arrow ON (nothing
  useful below 80 Hz on this mic), peaks **−12…−6 dBFS**. A take peaking at
  0.00 dBFS is clipped — recount before mixing (mic-compare has the checks).
- LUNA channel stays empty: Unison slot free, no Record FX. What the file
  holds must be the dry mic, or the chain fights an invisible one.
- 48 kHz everywhere. Take lands ~ −21 LUFS, floor ~ −60 dBFS, SNR ≈ 44 dB —
  worse than that, re-record rather than repair.

## The channel chain (REAPER FX slots, top to bottom)

| # | plugin | settings | job |
|---|---|---|---|
| 1 | SD Vocal (repair) | Plos On, Plos Thr −26, Amt 12, Freq 120 Hz, **Ess Amt 0** | kill plosives first; essing OFF here, it has its own slot |
| 2 | ReaGate | thr −45 dB, attack 2 ms, release 150, hold 60, **Dry −14 dB**, hysteresis −6, detector HPF 90 | room/breath down BEFORE compressors raise it; Dry −14 = soft expander, never a hole |
| 3 | SD EQ | HP 90 Hz | rumble out of every detector downstream |
| 4 | SD EQ | −2.5 dB @ 300 Hz, Q 1.2 | mud cut, subtractive before compression |
| 5 | UADx 1176 | Input −19.2 dB, Attack 3, Release 5, ratio 4:1 | peaks (3–5 dB GR on loud syllables only) |
| 6 | UADx LA-2A | Peak Reduction 44, Comp mode | levelling, 2–3 dB GR almost always |
| 7 | UADx 610-B | Line in, Hi EQ **10 kHz +4.5 dB** | tube colour + the air the AT2040 lacks (its top is −8 dB vs a bright mic at 8–12k; the shelf is cheap because a good take has ~36 dB SNR up there) |
| 8 | SD Vocal (de-ess) | Sub Mode 1, Ess Track 1, Thr −30, Amt 6 | after the boost that raised the esses |
| 9 | SD EQ | +2 dB @ 3.5 kHz, Q 0.7 | articulation |

All-SuperDuper fallback (no UADx): slots 5/6 = SD Compressor fast
(−13/4:1/0.8/60/Curve 1) then slow (−22/2.5:1/20/300/Curve 2), slot 7 =
SD Saturator Drive 8 Tube Mix 0.4 + shelf on slot 9's EQ (High 9k +4.5).

## Space — two returns, both ducked

Sends from the vocal: **Vox Delay −14 dB**, **Vox Reverb −9 dB**. Returns run
**Mix = 1.0** (100 % wet):

- SD Delay: Time Sync on, Feedback 0.35, Tone 4500, Duck 10 dB (5/180 ms)
- SD Reverb: Pre-Delay 40 ms, Decay 0.65, Size 0.6, Duck 6 dB (5/250 ms)

The duck hides the tail while a word sounds and lets it bloom in pauses —
wet pauses, dry words. **"Ревер не работает" almost always means the return
reset to defaults** (Mix 0.18, Duck 0): quitting REAPER without saving
loses parameter-only changes — verify Mix=1.0 before touching anything else.

## The beat under the voice

4-channel **Beat Bus**: every instrument's master send off, routed into the
bus; the vocal keys it via a **post-FX / pre-fader** send into channels 3/4
(`I_DSTCHAN=2, I_SENDMODE=3` — pre-fader so pulling the vocal fader never
kills the duck). On the bus: SD Compressor, preset **Voice Duck** (idx 6) =
thr −30, ratio 10, **Range 3 dB**, 12/350 ms, SC HPF 100. Range is the whole
point: depth is capped no matter how loud the take got.

Balance that worked: bus fader **−6 dB** static, duck adds up to 3 dB under
words. Two independent ears: fader = музыка всегда, Range = насколько ныряет.

## Master

SD EQ (HP 25) → SD Compressor (−24 dB, 2:1, 20/200, +3 makeup, Curve 2) →
SD Limiter (**Ceiling −1.2, True Peak on**, Input to taste ≈ 12.5).
Shipped: −10.4 LUFS-I, −1.1 dBTP. Master fader at 0 — a pulled-down master
fader silently eats the limiter's ceiling (found at −5.3 dB, looked like a
broken limiter).

## REAPER traps that cost hours here

- **CLAP params only land while the transport runs**: play → set → verify
  (`TrackFX_GetParam`) → stop. A silent failure otherwise.
- **REAPER maps the CLAP dylib per session.** After rebuilding a plugin,
  `lsof -p $(pgrep -x REAPER) | grep <name>` and compare the inode with
  `ls -i` on disk. The `[bNNNNN]` in a saved project's FX name LIES — three
  "sidechain is broken" hunts were a stale mmap, the code was fine.
- A stale `request_*.json` in `mcp_bridge_data/` replays on the next start —
  a leftover Quit request closes REAPER at launch and looks like a crash.
- Verify a chain survived a restart before re-tweaking it: enabled flags and
  return Mix values are what actually get lost.
