# verify — a mix change stays only if two independent instruments agree

**One instrument is not an argument.** The bus (`mix_masking`) listens to the live session:
it lags 50–100 ms and sees mutes, loops and whatever else the DAW is doing. The offline
render (`sdsp-chain` → `mixcheck.profile`) is deterministic and sees nothing the DAW adds.
Each has a blind spot the other does not, so `tools/mixverify.py` reads both and keeps a
change only when they move the same way on the question asked. One number is a reason to look, never to keep.

## The command

```bash
python3 tools/mixverify.py before.toml after.toml --question masking --labels before,after
python3 tools/mixverify.py before.toml after.toml --question balance --track "chord" \
        --labels pre,post --section 62 78 --ref ref.wav --ref-start 55 --ref-end 71 --json
```

The TOMLs are sdsp-chain mixes of the same inputs; the labels are `mix_masking(label=…)`
runs in `~/.superduper-dsp/mix-log.jsonl`, measured on the same loop. `--section A B`
limits the renders to [A, B) s; `--ref` prints band deltas against a reference (shown, not
judged). Binary: `--chain`, `$SDSP_CHAIN`, `$CARGO_TARGET_DIR`, `./target/release`.
Exit code 0 KEEP, 1 UNDO, 2 UNKNOWN. What each question reads, per instrument:

| `--question` | bus (instrument 2) | render (instrument 1) |
|---|---|---|
| `masking` | the pair's clash share (`--pair A,B`; default the worst clash in the before run) | the pair's separation in the clash band on the two **stems** (each rendered with the others muted, no master): \|E_a − E_b\| dB. "Within 6 dB" is the bus's definition of a clash, so wider is the same question asked offline |
| `balance` | the track's level | the track's stem RMS |
| `loudness` | the Master level | integrated LUFS of the mix |
| `mono` | nothing: `mix_phase` is not logged, so the verdict stops at UNKNOWN | worst per-band mono loss; correlation and side/mid < 120 Hz printed |

## Thresholds (a smaller move is "no change", not evidence)

| | value | applies to |
|---|---|---|
| level | **1 LU / 1 dB** | integrated LUFS, track and Master levels, stem RMS |
| band | **1.5 dB** | any mixcheck band, stem separation, per-band mono loss |
| share | **10 points** | a clash pair's share of the listened time |

## What the verdict means

- **KEEP**: both instruments moved by at least their threshold, the same way (better for
  `masking`; for `balance`/`loudness` the same direction, `--expect up|down` names yours), and
  no guard tripped.
- **UNDO**: the instruments disagree (one moved, the other did not, or opposite ways), both
  got worse, or a guard tripped: a band outside the question moved > 1.5 dB, LUFS moved > 1 LU
  on a `masking`/`mono` change, mono loss worsened > 1.5 dB in a band, another clash grew or
  appeared by ≥ 10 points, or (`balance`) a track you did not touch moved > 1 dB on the bus.
- **UNKNOWN**: an instrument is missing (no `--labels`, no log entry, no stem, no binary) or neither saw a change. Never KEEP on one.

## The three known-answer controls (`python3 -m pytest tools/test_mixverify.py`)

Fixture: a 55 Hz bass with harmonics (its 5th and 6th sit at 275 and 330 Hz) and a chord
cluster whose lowest tone, 300 Hz, is planted within 6 dB of them. The bus side is a fake log;
without the sdsp-chain binary the render controls skip and say how to build it.

1. **KEEP**: "after" cuts the chord 6 dB at 300 Hz (`eq` Mid Freq/Gain/Q), log 72 % → 30 %.
   Render: separation at 250–340 Hz +6.0 dB (chord stem −6.0, bass 0.0), LUFS −0.2 LU. Both agree.
2. **UNKNOWN**: identical TOMLs and two identical runs: 0.0 dB and 0 points, nothing to keep.
3. **UNDO**: identical TOMLs but the log claims 72 % → 30 %. The bus says better, the render
   says nothing moved: a log line alone does not keep a change.
