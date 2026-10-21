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
- [ ] Compressor: detector from the key track in a band (lo/hi Hz)
- [ ] EQ: dynamic band keyed by the bus (cut only while the key plays there)
- [ ] Reverb / Delay: duck the tail while the key plays (MS ch17)
- [ ] Vocal: rider — hold the vocal a set dB over the mix (MS ch19)
- [ ] Filter: key-driven cutoff — close/move while the key plays (pad out of the vocal's way)
- [x] sdsp-mix skill: when to key what; measure with mix_compare (Soothe/duck_under; extend per plugin)
- [x] MCP-first: generic `params` doc in all 31 plugins; plugin_params / plugin_set_params / duck_under

- [x] fader blind spot: mix_masking fader_db (Pi sits post-fader; the bus is pre-fader)
- [ ] phase interactions between tracks (Sound Radix Pi): each instance also
      publishes a short low-band (<~800 Hz) audio window; the bus computes per
      pair how much energy the sum loses (kick vs bass, layered kicks, mics);
      fix with polarity / allpass rotation. Needs a bigger slot → bus VERSION 2

## Done
