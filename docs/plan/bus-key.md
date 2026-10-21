# Bus key — every plugin can react to another track, no routing

Any SuperDuper plugin picks a **key track** from the bus; its publisher thread
(already awake every 50 ms) copies that track's 32-band spectrum into atomics;
the audio thread reads atomics. No host side-chain routing, no audio-thread
cost. Latency ~50–100 ms (publish period + analysis window): for slow ducking
(guitar under vocal, pad under kick with 100 ms+ release), not kick pumping —
the real side-chain input stays for that.

## Tasks
- [x] synth-core: `LiveScope.key_track` + `key_bands[32]`, filled by the
      publisher thread (own instance excluded); generic mailbox doc `key`
      (get/set track) for every plugin; `key_picker` GUI widget; state helper
- [x] Soothe: keyed spectral ducking (Trackspacer) — per band cut grows with
      the key's level in that band; params Key Depth / Key Range; mix with the
      resonance mode
- [x] Compressor: key detector in a band + Dual detector (RMS + peak stages, Peak Crest)
- [x] EQ: dynamic Mid band keyed by the bus or by itself
- [ ] Reverb / Delay: duck the tail while the key plays (MS ch17)
- [ ] Vocal: rider — hold the vocal a set dB over the mix (MS ch19)
- [ ] Filter: key-driven cutoff — close/move while the key plays (pad out of the vocal's way)
- [x] sdsp-mix skill: when to key what; measure with mix_compare (Soothe/duck_under; extend per plugin)
- [x] MCP-first: generic `params` doc in all 31 plugins; plugin_params / plugin_set_params / duck_under

- [x] fader blind spot: mix_masking fader_db (Pi sits post-fader; the bus is pre-fader)
- [x] phase interactions, detection on bus v1: mix_phase (master vs sum of tracks per band, broadband removed); fix = polarity/nudge via REAPER, measured
- [ ] phase, automatic (Sound Radix Pi): each instance also
      publishes a short low-band (<~800 Hz) audio window; the bus computes per
      pair how much energy the sum loses (kick vs bass, layered kicks, mics);
      fix with polarity / allpass rotation. Needs a bigger slot → bus VERSION 2

- [x] Reverb/Delay Key Duck, Filter Key Mod, Vocal Rider (synth-core::bus_key)
- [x] NEW SuperDuper Multiband (bit-exact idle, bands on the spectrum, per-band bus key; 8th-order shelves + Newton solver after review)
- [x] Saturator Inflate (Oxford Inflator curve)

- [x] tools/sdsp-chain: Multiband, Morph, Double registered; tools/plugin_smoke.py renders all 24 (sdsp-mash still to do)
- [x] skills: Morph + Double rows in sdsp-mix
- [x] NEW SuperDuper Morph (any timbre onto any performance; Sidechain / Captured / Model)
- [x] NEW SuperDuper Double (drifting PSOLA heads, Tuned modes, Key Open)

- [ ] genre target curve (Ozone Tonal Balance, transparent): `mixcheck --target refs/*.json` — the corridor (mean ± spread) of 5–10 reference choruses per band; the agent moves EQ/Multiband only where the mix leaves it

- [ ] StftProcessor reports N−hop but delays N (measured by impulse and by Morph): Pitch (Track mode) and Vocoder (Spectral) tell the host 512 samples (10.7 ms) too little; fix = separate ring offset from reported latency + re-align their dry paths, re-record snapshots

## Done
