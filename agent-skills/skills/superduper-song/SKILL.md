---
name: superduper-song
description: "Make songs and tracks in REAPER driven by the SuperDuper DSP instruments (Wave, Kubyz, Drum, Pad, Sampler, Ambient) and effects through a REAPER MCP server. Covers exact parameter maps by name, preset-recall indices, the raw-value gotcha, CC expression maps, VST3i versus CLAPi, and audio-to-MIDI melody extraction. Use when the user says \"make a track with our plugins\", \"build a beat on Drum\", \"write a melody on Wave or Kubyz\", \"load the Aulos preset\", \"tweak the parameters remotely\", or \"turn this audio melody into MIDI\". Living skill: the maps and recipes grow each session. Do NOT use for mixing or mastering an existing render (sdsp-mix)."
---

# superduper-song: make tracks in REAPER with the SuperDuper instruments

> **Needs** REAPER 7+ with the SuperDuper DSP CLAP plugins installed ([releases page](https://github.com/fortunto2/superduper-dsp/releases)),
> and a REAPER MCP server connected to the agent (one that exposes `dsl_*` and `track_fx_*` tools; this plugin does not include it).
> Python with `numpy`, `librosa`, `mido` only for the optional audio-to-MIDI recipe. Check first: look for the REAPER tools in the agent's
> tool list, and on macOS `ls ~/Library/Audio/Plug-Ins/CLAP | grep SuperDuper`. If REAPER or the MCP server is missing, say so and stop;
> do not try to drive REAPER by other means. Tool names below (`dsl_*`, `track_fx_*`) are those of the REAPER MCP the skill was written against.

The plugins exist for this: an agent or DAW drives them headless through a REAPER MCP server. Everything exposed as a CLAP parameter can be
set remotely; no per-plugin MCP is needed, because the REAPER MCP addresses parameters by `track_index, fx_index, param_index`.
General REAPER MCP mechanics (tracks, MIDI, markers, render) belong to that server's own documentation. This skill covers only what is specific to the
SuperDuper instruments and to building a song with them.

## Critical: `track_fx_set_param` takes the RAW value, not 0..1

Verified live (2026-07-24). `track_fx_get_param` returns `value (range: min to max)` in real units, and `set_param` writes in the same units:
- Cutoff (Hz): `set_param(param=4, value=1800)` is 1800 Hz, not 0.18.
- Preset (index): `set_param(param=37, value=14)` is preset 14, not "normalized 1.0" (that would give index 1).
- Levels and mixes of 0..1 are written as 0..1; dB as dB; ST as semitones.

(Some REAPER MCP docs say "params normalized 0..1"; for this bridge that is wrong.)

Always check: `track_fx_get_param(track, fx, idx)` prints both the value and the range. Treat the range as the truth.

## VST3i or CLAPi (matters after a rebuild)

`track_fx_add_by_name "SuperDuper Wave"` fuzzy-matches and loads the **VST3i** wrapper. Usually fine, same code. But:
- The display name carries a build number: `SuperDuper Wave [bNNNNN]`. A freshly built binary has the LARGEST number.
- After a plugin rebuild REAPER keeps the OLD binary in already-open instances; **a rescan (Prefs, Plug-ins, CLAP, Clear cache + rescan) registers the new .clap but does NOT replace loaded instances.**
- To be sure of fresh code, add with the prefix **`CLAPi: SuperDuper <Name>`** (loads the .clap directly). Check `track_fx_get_name`: the build number must match the fresh binary (`strings <bundle>/Contents/MacOS/<Name> | grep -oE "\[b[0-9]+\]"`).
- The instrument MUST be FX 0 (before effects). `add_by_name` appends, so move it without losing the chain: `track_fx_copy_to_track(source=T, fx_index=last, dest_track=T, dest_fx_index=0, move=true)`.

## Instruments and parameter maps (index, name, range)

All are MIDI instruments (note in) except Ambient (a generator). Each one's last parameter is **Preset** (stepped; recalls the whole timbre or kit in one write; the value is the RAW index).

### SuperDuper Wave: wavetable synth (P_PRESET=37)
`0 WT Pos(0..1) · 1 Unison(1..7) · 2 Detune(0..50ct) · 3 Sub(0..1) · 4 Cutoff(30..18000Hz) · 5 Resonance(0..0.9) · 6 Filter(0..2 mode) · 7 Drive(0..1) · 8 Attack(0.001..4s) · 9 Decay(0.01..4s) · 10 Sustain(0..1) · 11 Release(0.01..8s) · 12 Output(-36..6dB) · 13 Anti-Alias · 14 Noise(0..1) · 15-19 FEnv(Amt/A/D/S/R) · 20 LFO Rate(0.05..30Hz) · 21 LFO Depth · 22 LFO Shape(0..3) · 23 LFO Dest(0..2) · 24 Bend Range(ST) · 25-26 LFO Sync/Div · 27-32 Mod1/Mod2(Src/Dst/Amt) · 33-34 Sync/Ratio · 35-36 FM Ratio/Amt · 37 Preset`
Presets (index): `0 Init(Sine) · 1 Saw · 2 Square · 3 Triangle · 4 Pulse25 · 5 Sine>Saw · 6 Sine>Square · 7 Saw>Square · 8 Triangle>Saw · 9 Reese Bass · 10 FM Growl · 11 808 Sub · 12 Fat Saw Lead · 13 Formant · 14 Aulos`
The waveform is part of the preset, so reese, 808, formant and aulos are reachable only through the Preset parameter (or the GUI), not through a separate parameter.

### SuperDuper Kubyz: physical-model jaw harp / khomus (P_PRESET=19)
`0 F1(80..1500Hz) · 1 F2(200..3500) · 2 F3(600..6000) · 3 VoxMix(0..1) · 4 Vel Shift(0..0.5) · 5 Bright(0..2) · 6 Attack(0.001..2s) · 7 Decay(0.01..4s) · 8 Sustain(0..1) · 9 Release(0.01..4s) · 10 Output(dB) · 11 Tongue ST(-36..36) · 12 Mouth Shp(0..4) · 13 Mouth Rate(0.05..20Hz) · 14 Mouth Dep · 15 Mouth Stereo · 16 Bend Range · 17-18 M Sync/Div · 19 Preset`
Presets: `0 Init(sine) · 1 Bashkir Kubyz · 2 Khomus Sample · 3 Real D2 · 4 Aulos`. Pitch of the Bashkir kubyz is A#2, the Yakut khomus F#2, the Tatar kubyz C#2.

### SuperDuper Drum: 6 synthesised voices (P_PRESET=27)
Voices Kick/Snare/HHc/HHo/Clap/Cowb, each with 4 parameters `Tune(ST) · Decay(s) · Level(0..1) · Pan(-1..1)`, indices 0..23 (voice x 4 + field). Then `24 Drive · 25 Master(dB) · 26 Note Out · 27 Preset`. GM notes 35-57 (Kick=36, Snare=38, HHc=42, HHo=46, Clap=39, Cowb=56). Presets: Trap, Boom Bap, 808 Sub, Techno, Joy Division, Boards of Canada. Notes outside the map are forwarded to the CLAP note-out (route them into Wave or Kubyz).

### SuperDuper Pad: polyphonic synth (P_PRESET=14)
`0 Cutoff(80..16000Hz) · 1 Resonance(0..0.9) · 2 Modulation(0..50ct) · 3 Drive · 4 Width(0..30ct) · 5 Attack · 6 Decay · 7 Sustain · 8 Release · 9 Output(dB) · 10 Bend Range · 11 Env Delay · 12 Env Hold · 13 Polyphony(2..16) · 14 Preset`

### SuperDuper Sampler: WAV player with YIN tuner (polyphonic)
`0 Sample(0..255) · 1 Root(0..127) · 2 Tune(ST) · 3 Fine(ct) · 4 Loop · 5-6 Loop Start/End · 7-10 ADSR · 11 Output · 12-13 Start/End · 14 Reverse · 15 Filter(0..4) · 16 Cutoff(0..127) · 17 Reso · 18 Env>Cutoff · 19 Vel>Amp · 20 Vel>Cut`. The plugin scans `~/Music/SuperDuper Samples/` and `~/Music/Favorite 808s/` for WAVs; choosing a WAV is the `Sample` parameter (an index into the scanned list).

### SuperDuper Ambient: autonomous drone generator (no MIDI). Add it to a track and it plays by itself.

## Preset recall (a whole timbre in one write)

`set_param(Preset_idx, RAW_index)` switches the preset with everything in it: wave, kit, formant, envelope. But:
- **Recall is applied reliably ONLY during playback** (`dsl_play` before or during). On an idle track the event may never reach the plugin. Order: `dsl_play()`, then `set_param(Preset, idx)`, then verify.
- To verify, read a parameter the preset overrides (e.g. Wave Aulos: Unison must become 3, Cutoff 1800, Detune 12). If it still shows the default, recall did not happen (usual causes: idle track, or a normalized 1.0 passed instead of an index).
- `apply_preset` marks all parameters dirty, so the host and automation see the recall.

## Real-time expression (CC)

The synths listen to MIDI CC for live playing (CC written into a MIDI clip does not mark parameters dirty, so there is no feedback loop):
- **CC7 (Volume) drives the output VCA** on Wave and Kubyz (added 2026-07-24): sustained breath or swell by volume; neutral (1.0) until a value arrives.
- Wave: CC1 to LFO Depth, CC11 to Cutoff, CC71 to Resonance, CC74 to WT Pos, aftertouch to LFO Depth, pitch bend to +-Bend Range.
- Kubyz: CC1 to Mouth Depth, CC2 to Mouth Stereo, CC11 to F1, CC71 to F3, CC74 to F2.
- Pad/Wave/Kubyz: pitch bend, MIDI CC and a tempo-synced LFO (host BPM from the transport event).
The envelope (Attack/Release) gives a wind-like "breath" to a note when you are not driving a CC swell: a slow Attack makes the note swell in, a slow Release lets it fade.

## Building a song: the usual flow

1. **Tempo:** `dsl_set_tempo(bpm)`. Trick: `bpm=60` makes 1 beat = 1 second, so MIDI `start/length` in seconds read as timeline seconds. Check the real tempo with `Master_GetTempo` through the bridge (`dsl_get_tempo_info` reports 120 wrongly).
2. **Tracks:** `dsl_track_create(name, role)`. Warning: it **PREPENDS**; every new track lands at index 0 and the order inverts. Create in reverse order, or rename by index afterwards.
3. **Instrument:** `track_fx_add_by_name(track_idx, "SuperDuper Wave")` (or `CLAPi: ...` for a fresh build). It must be FX 0.
4. **Timbre:** either one preset recall (`set_param(Preset, idx)` during playback) or single parameters from the maps above. Anything continuous can be set without a preset; waveform, kit and formant only come with a preset.
5. **MIDI:** `dsl_midi_insert(track, time={"start":s,"end":s}, midi_data={notes:[{pitch,start,length,velocity}]})`. `start/length` are SECONDS from the start of the item. For many notes use a bulk route in the bridge (create the MIDI item, then insert notes in a loop); do not inline thousands of notes into one tool argument.
6. **FX chain:** after the instrument, `track_fx_add_by_name(track, "SuperDuper Reverb")` and so on. Available: Eq, LinEq, Compressor, Saturator, Limiter, MidSide, Reverb, Supermass, Delay, Chorus, Filter, Soothe, Vocal, NAM, Spectrum. The order is the order of adding; keep the instrument at 0.
7. **Arrangement:** markers and regions with `dsl_marker` / `dsl_region` (seconds or "16 bars").
8. **Master:** an in-REAPER chain on the master, or headless with the `sdsp-chain` skill (EQ, compressor, saturator, limiter, per-stage LUFS and dBTP). The meter is the SuperDuper Spectrum plugin (LUFS and true peak).

## Recipe: audio to MIDI (re-play a melody from a recording)

Used for an Odysseus theme. Pull the notes out of audio and insert them as MIDI under a SuperDuper instrument:
1. `pyin` (librosa) over the chosen stretch: `f0 = librosa.pyin(y, fmin, fmax, sr, frame_length=4096, hop_length=512)`. A narrow register (fmin/fmax) removes the detector's octave jumps.
2. Pitch class via `chroma_cqt` (salience per note): resolves octave ambiguity and finds the tonic.
3. Segment into stable notes (adjacent voiced frames within about a semitone, length 0.3-0.5 s or more).
4. Velocity = the RMS peak of each note (`librosa.feature.rms`, normalised to the 99th percentile).
5. Insert with `dsl_midi_insert` at bpm=60 (start in seconds). Microtonal live or ethnic instruments (aulos, kubyz) sit near equal-tempered pitches; fine-tune the instrument's pitch by ear at the end.
6. Take the original's spectrum (`np.fft.rfft`: harmonics relative to the fundamental, centroid, beating) and translate it into synth parameters (detune/unison for beating, cutoff for centroid, drive for richness of overtones, noise for breathiness). The Aulos preset was built this way.
Environment: any native Python 3.11+ (`python3 -m venv`, `pip install numpy librosa mido`), not a Pyodide shim.
For a full transcription of a recording use the `audio-to-midi` skill instead.

## Gotcha summary

| Symptom | Cause | Fix |
|---|---|---|
| A preset or parameter changes nothing | value given normalized, but a RAW index or unit is needed | Preset=14 (not 1.0), Cutoff=1800 (not 0.18) |
| Preset recall did not apply | idle track | `dsl_play`, then set Preset; check an overridden parameter |
| Plugin sounds old after a rebuild | REAPER holds the old binary | add again as `CLAPi:`; compare the build number in the name |
| Instrument sounds after the effects | add appends | `copy_to_track(move=true, dest_fx_index=0)` |
| Tracks in reverse order | dsl_track_create prepends | create in reverse order or rename by index |

## Related
- `sdsp-chain`: headless mastering with the SuperDuper effects, LUFS and true peak.
- `sdsp-mash`: mashups and cyphers.
- `superduper-plugin`: create or change the plugins themselves.
- `audio-to-midi`: transcribe a recording into MIDI.
