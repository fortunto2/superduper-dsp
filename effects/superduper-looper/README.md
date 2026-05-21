# SuperDuper Looper

Mobius-style 4-track live looper. 60 s/track, host-BPM sync with
bar-aligned quantize, per-track Feedback for tape-style overdub
decay, MIDI CC control for hands-free hardware triggering.

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes (4 stereo tracks) |
| Sidechain | no |
| Latency | 0 samples |

## Tracks

Four independent loop tracks. Each has its own state machine:
**Empty → Recording → Playing → Overdubbing → Stopped**. Track strips
show:

- Title + colour-coded state (REC red, Play green, Overdub yellow, Stop grey)
- Loop position progress bar
- Level, Feedback, Mute knobs/toggles

## Sync

**Sync** ON locks loop length to host BPM via the **Bars** setting
(1/2, 1, 2, 4, 8 bars). Records start on the next bar boundary —
guarantees alignment even if you press Rec slightly off-beat. Free
mode = manual length.

## Per-track Feedback

Like a tape loop — every overdub pass multiplies the existing audio
by `Feedback`. 1.0 = perfect retain, 0.95 = slow decay (classic tape
echo wear), 0.5 = fast fade (each new layer dominates).

## MIDI CC map (any channel)

| Action | CC range |
|---|---|
| Rec | 20-23 (T1-T4) |
| Play/Stop | 24-27 |
| Overdub | 28-31 |
| Clear | 60-63 |

Press = value ≥ 64. Wire a hardware foot controller or any DAW MIDI
clip to control loops hands-free.

## Parameters

| Group | Params |
|---|---|
| Master | Sync, Bars, Dry, Master |
| Per-track (×4) | Level, Feedback, Mute |
