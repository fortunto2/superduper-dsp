# SuperDuper Space

A track's place in the room. Stereo (or mono) in, up to 12 channels out —
`L R C LFE Ls Rs Lb Rb Tfl Tfr Tbl Tbr`, the order REAPER, Logic and the ITU
tables share — and a map you drag the source around on. Not a Dolby Atmos
renderer: an Apple Spatial delivery still needs an ADM BWF master from
Dolby's tools; this is the placement that feeds such a bus, or a 5.1 / 7.1
mix, or (with Monitor on) a wide stereo panner.

## How

Vector-base amplitude panning (Pulkki 1997) on the layout's bed ring: the
source azimuth picks the two speakers either side of it and their gains are
the inverse of that pair's base, equal-power normalised — a source exactly on
a speaker is only that speaker, a source between two is the same loudness
everywhere. 7.1.4 adds a height ring that `Z` blends in (sin² law). `Spread`
pulls the point towards a little of every speaker; `Distance` takes level
(−12 dB at the far end) and air (a one-pole down to 4 kHz). A stereo input
places L and R `Width × 15°` either side of the position. Every gain is
smoothed, so a move never clicks. No latency.

## Layouts

Stereo (L −30°, R +30°) · Quad (±45°, ±135°) · 5.1 (L R C LFE Ls ±110°) ·
7.1 (Ls ±90°, Lb ±150°) · 7.1.4 (+ Tfl Tfr ±45°, Tbl Tbr ±135° overhead).
The output port is always 12 channels wide; a smaller layout leaves the rest
silent. On a REAPER track set the track channel count to 12 (or 8 for 7.1)
and route the channels to your surround bus.

## Parameters

Layout · X (−1 left … 1 right) · Y (−1 back … 1 front) · Z (0 … 1 overhead,
7.1.4 only) · Spread · Distance · Width · LFE (send, off at −60 dB) · Output ·
Monitor (Off / Stereo fold: every speaker folded onto channels 1–2 by its
azimuth, to hear the placement on headphones or a stereo track).

## Agent access (MCP-first)

`plugin_get(track, "Space", "position")` → `{x, y, z, spread, distance,
azimuth_deg, layout, layouts[], speakers[{channel, name, azimuth_deg, height,
gain}]}` — the gains are what the last block used, so the agent sees the
placement as sound. `plugin_set(track, "Space", "position", {"x": 0.6, "y":
-0.3, "layout": "7.1.4", "z": 0.5})` moves it; `plugin_params` /
`plugin_set_params` reach every parameter. The bus scope carries the stereo
fold, so the mix map still shows the track.

## Tests

`cargo test --release -p superduper-space`: a source on a speaker is only
that speaker (every layout); between two is equal-power and only those two;
height moves energy to the top ring and nowhere else on 7.1; Spread 1 is every
speaker alike; distance cues; azimuth from the map; the `position` document
round-trips; through a real CLAP host the default placement lands on the
centre speaker alone, hard right on Rs, and Monitor folds onto channels 1–2.
