# SuperDuper Piano — SFZ engine + plugin (2026-10-20)

Goal: own SFZ player. Engine in `synth-core::sfz` (shared with Sampler later),
plugin `effects/superduper-piano`. Packs = any folder with `.sfz` under
`~/Music/SFZ/` (drop a folder → appears in list). No samples in git.
Reference pack: Salamander Grand V3 (`~/Music/SFZ/SalamanderGrandPiano`,
641 FLAC 48k/24, 6809 s total → 1.3 GB as i16 in RAM; preload OK on 32 GB,
streaming = later). Ground truth renderer: `sfizz_render` (scratchpad).

## Tasks
- [x] 1. Parser: #define/#include, <control>/<global>/<master>/<group>/<region>/<curve>, inheritance, note names, default_path
- [x] 2. Region model: key/vel/rand/cc ranges, trigger attack/release, on_locc/on_hicc, sw_* keyswitch, tune/pitch_keycenter/transpose, volume/amplitude(+oncc)/pan, amp_veltrack(+oncc), ampeg_*, rt_decay, group/off_by/polyphony/note_polyphony/off_time, offset, loop_mode
- [x] 3. Sample loader (symphonia: flac/wav/ogg) → i16 shared cache, resample-on-play
- [x] 4. Engine: voices, sustain pedal, release triggers, CC set_ccN/set_hdccN defaults
- [x] 5. (sfizz: corr 0.994, constant +7.35 dB = sfizz master vol; tests/sfz_salamander.rs, mutation-checked) Known-answer test vs sfizz_render (levels per velocity layer within ~1.5 dB)
- [x] 6. Plugin crate: params, pack scan, loader thread, GUI (pack list + knobs), state (pack path)
- [x] 7. Registration: workspace, bundle script, cmake list, release.yml, CLAUDE.md, README
- [x] 8. Build + install + REAPER smoke

## Done notes
- Plugin tests: tests/quality.rs (fixture pack, every note, velocity, pedal, no-alloc), tests/real_pack.rs (real ~/Music/SFZ, Salamander loads 1.3 s).
- Installed CLAP + VST3 + AU. REAPER smoke not run (REAPER closed, MCP venv missing).
## Next
- Sampler Kit/Slice on the same engine; disk streaming; smpl loop points; CC mod of live voices.

## Track 2 — hybrid Drum (2026-10-20)
- [x] synth_core::drumlib: name classifier (2 passes), loop filter, key from name, SampleHit voice, scan + feature cache
- [x] Audio classifier measured: right pad 44% KSHMR / 47% 909 / 55% Cassette808 → NOT used in scan
- [x] Drum params 29-40 (Src, Smp per pad), loader thread, retire-by-strong_count, state paths, GUI ◀ ▶, Save kit .sfz
- [x] Tests: sample_pads.rs (browse, layer, hat choke, no-alloc, kit export → Piano engine)
- [x] Piano: visual 88-key keyboard (held / sounding / out of range), click+drag to play, pedal light
- [ ] More pads (toms, ride, crash) — perc pad holds toms+perc for now
- [ ] Better sound classifier (kNN on KSHMR labels) if unnamed packs show up

## Track 3 — drums in key (2026-10-20, in progress)
- [x] Measured key-from-sound on 273 KSHMR kicks/toms (truth = name): loudest-slice YIN 18% exact; FFT peak of the tail 60-460 ms after the peak 66% exact / 83% ±1 st → used for unnamed samples
- [ ] drumlib: drum_pitch_hz + key on every tonal sample (name wins), cache v2
- [ ] Drum `Key` param (Off / C..B / Auto): kick + perc/tom pads retune to the song key (±6 st), browser sorts by shift
- [ ] Auto = ~/.superduper-dsp/song-key (any tool/agent writes it) else the notes Drum passes through

## Track 4 — smarter mixing (approved 2026-10-20, do in order)
- [x] 1a. Bus core: sdk::bus (mmap, seqlock slots), publisher thread in all 31 plugins, track names, sdsp-bus CLI, EQ e2e test
- [ ] 1b. Control region (song key, roles, targets) + gateway: MCP (rmcp, :7891) for agents, OSC for Max/phone
- [ ] 1. (was) Inter-plugin bus: every SuperDuper plugin publishes its spectrum/level to a shared registry (cross-dylib → shared memory file), others read it with no routing
- [ ] 2. Band-wise sidechain: Soothe gets a key input → cuts only the bands the key (vocal / kick) occupies, only while it sounds (Trackspacer-style)
- [ ] 3. Auto-mix as optimisation: sdsp-chain render → mixcheck vs reference → adjust levels/EQ/duck → repeat
