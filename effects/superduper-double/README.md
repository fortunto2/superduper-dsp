# SuperDuper Double

A vocal doubler that behaves like a second take, not like a chorus: each
double drifts slowly and independently in time and in pitch, with the
formants kept, so it never turns into a static micro-shift or a flanger.

## How

Up to 4 doubles are TD-PSOLA read heads over one shared input ring and one
YIN pitch tracker. Time drift picks *which grain* to copy (it is never a
delay line, so a 30 ms wander produces no Doppler pitch bend); pitch drift is
a few cents of PSOLA shift. Each head has its own band-limited noise
(0.3–1 Hz) for both, its own level (0 to −2.5 dB) and pan (spread over
±Spread).

## Modes

- **Natural** — the drift described above.
- **Tuned** — the doubles are snapped to `Key` / `Scale` at `Retune` speed
  (Pitch Var off); the lead stays untouched. The live-voice-plus-perfect-shadow
  sound.
- **Tuned Lead** — the reverse: the lead goes through the pitch engine and is
  snapped, the doubles stay natural. A 20 ms crossfade engages it.

## Parameters

Voices (1–4) · Timing (ms of drift, the double wanders between 0.3 and
1 × Timing) · Pitch Var (cents) · Formant (small shift) · Spread · Level ·
Dry (0 = print only the doubles) · Mix · Output · Seed · Mode · Key · Scale ·
Retune · Key Open / Key Range (the doubles' level follows a bus key track,
e.g. swell in the chorus; 0 = off, and with no key picked the doubles play
as at 0, so "Chorus Swell" is audible before a key is chosen).

Latency 2744 samples (57 ms at 48 kHz; the pitch engine's, same as Pitch and
Tune). Mix 0 or Voices 0 = the input delayed by that, bit for bit.

## Agent access (MCP-first)

`plugin_params` / `plugin_set_params(track, "Double", {"Voices": 2, "Mode": 1})`;
the bus key via `plugin_set(track, "Double", "key", {"track": "Master"})`.

## Tests

`cargo test --release -p superduper-double`: Mix 0 bit-exact (also through a
real CLAP host); a 220 Hz sine's double stays within ±Pitch Var and wanders;
measured timing offset varies inside the Timing range; formant peaks of a
synthetic vowel move < 1 %; Tuned turns 223 Hz into 220 within 0.14 cents;
voices with different seeds are decorrelated (r ≈ −0.01); no allocation in
`process()`; quality snapshot.
