# SuperDuper Multiband

2–4-band compressor that does nothing to the sound until a band actually
compresses, and lets each band react to another track from the SuperDuper
bus — no side-chain routing in the DAW.

## How it is transparent

The audio never passes through a crossover. Band gains are applied as a
chain of 8th-order low shelves sitting at the crossover frequencies; a
shelf whose two neighbours want the same gain is skipped, so with nothing
compressing the output is the input bit for bit (measured −240 dB on 2, 3
and 4 bands). Linkwitz-Riley crossovers only feed the detectors and Solo.
Because shelves overlap, a Newton solver on the exact shelf response finds
the shelf gains that put each band's *measured* gain where its detector asks
(−12 dB requested on a ⅓-octave band measures −11.99); the meter reports the
gain the audio got, not the detector's wish.

## Parameters

| Param | Range | What |
|---|---|---|
| Bands | 2–4 | band count; crossovers (draggable on the spectrum) keep ≥ ⅓ octave apart |
| Xover 1–3 | Hz | crossover frequencies |
| per band: Threshold, Ratio, Attack, Release, Knee, Makeup, Range | | the usual; **Ratio below 1:1 = upward** (quiet parts come up, limited by Range) |
| per band: Key | 0..1 | blend the detector from the band's own signal toward the bus key's level in the band's range |
| per band: Solo, Bypass | | listen to one band; leave one band untouched |
| Mix, Output | | parallel blend, trim |

Zero latency. Solo and Bands changes crossfade (5 ms); envelopes of a
re-enabled band restart at 0.

## The bus key

Pick a key track (`key:` in the GUI, or `plugin_set(track, "Multiband",
"key", {"track": "Голос"})` from an agent). Each band reads the key's level
over its own frequency range, so one band can duck under the vocal while the
others compress the track's own signal. The bus lags 50–100 ms: slow ducking,
not kick pumping (that is a side-chain compressor's job).

## Agent access (MCP-first)

Every parameter: `plugin_params` / `plugin_set_params(track, "Multiband",
{"B2 Threshold": -24})` (names as `plugin_params` lists them). Meter doc:
`plugin_get(track, "Multiband", "meter")` → per band GR now, max over ~0.7 s,
detector and key level.

## Presets

Mastering Glue · De-Mud Low-Mid · Vocal De-Harsh · Duck Under Key · Bass
Control.

## Tests

`cargo test --release -p superduper-multiband`: crossover reconstruction,
one band only touches its band, narrow bands get the gain they ask for and
the meter matches, keyed band reacts to the key with a quiet input, unsolo
and band-count changes do not click, no allocation in `process()`, quality
snapshot.
