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

- [ ] NEW SuperDuper Space (spatial panner, asked 2026-10-21): an object position (x, y, z) per track →
      VBAP/DBAP gains to N speakers (stereo, 5.1, 7.1.4, custom layouts as REAPER's ReaSurroundPan
      exposes them) + distance cues (level, air LPF, early reflection) + optional ambisonic encode,
      binaural (HRTF) monitor for headphones; CLAP multichannel ports; MCP-first so the agent
      places tracks on the map and the bus map shows the room. Not a Dolby Atmos encoder (an
      Apple Spatial Audio delivery needs an ADM BWF master from Dolby's renderer / Fiedler Composer)

- [ ] bypassed plugin = silence on the bus: every plugin's bypass path copies dry and returns before
      `scope.push`, so a bypassed Morph read −120 and `prefer: Morph` lost the 808 (measured 2026-10-21).
      Fix in the plugins: push the dry signal on bypass (31 process() paths); until then serve.rs
      falls back to the loudest instance when the preferred one is silent

- [ ] Morph: side-chain f0 on a jaw-harp drone read 943 Hz / unvoiced (the formant, not the
      fundamental ~130 Hz), so Harmonic mode fell back to Envelope (measured 2026-10-21 on
      "Kubyz Drone C"). Add a target f0 hint / use the bus key's pitch, or a subharmonic check in
      the YIN pass for strongly formant-ed sources

- [x] clap-validator (tools/validate_claps.py, `make validate`, pre-commit via `make hooks`), first run 2026-10-21:
      - [x] param-conversions in 21 plugins — fixed 2026-10-21 without per-plugin tables: every
        plugin's display moved into one `display_text<W: fmt::Write>` and `text_to_value` falls
        back to `ParamDef::text_to_value_by_display` (scan the integer steps of a ≤512-step param
        through the display; wider ranges try the number in the text and both ends, "random #731"),
        so the entry accepts exactly what the display shows. Drum 'Key': "Auto (…)" is checked
        before the key-name parser. 28 plugins converted; the 6 the DSP agent owns follow
      - [x] Preset selector (9 synths): the param is a label now; recall = GUI list, the
        `preset` mailbox doc (`bus_mail::preset_request`) or `clap.preset-load/2`
        (`sdk::preset_load`, what sdsp-chain uses). `preset_recall_target` is gone. Under
        clap-validator 0.4 this was 5 tests × 3 plugins (param-set-events, no-cookies, three
        state-reproducibility); under 0.3.2 only Pad/Wind showed it
      - [x] clap-validator 0.4.1 (rustc 1.99): new JSON shape read; "a validated bundle with no
        test entries is UNKNOWN" (the 0.3 reader printed 0 failing over 34 real failures);
        every plugin's `load()` ends in `track_watch.rescan_param_values()` (CLAP: values
        changed under the host ⇒ `params.rescan(VALUES)`; 34/34 failed state tests without it);
        factory state for the run (`SUPERDUPER_{WAVE,KUBYZ,NAM}_FACTORY=1`: Wave/Kubyz boot on
        the user's last preset, which read as a wrong default); Wind/Kubyz table defaults =
        their init preset; RBJ ω₀ clamped below Nyquist (NaN at a 1234 Hz rate in Kubyz, Wind,
        Vocal); Morph saves the model the audio thread would use
      - [ ] Drum `-inf` at a 1234.57 Hz sample rate (process-varying-sample-rates, sample 179):
        not reproduced by tests/odd_sample_rate.rs (every key, velocities down to 0, six rates);
        the validator's note generator also sends note-offs, chokes and sample offsets −4..64 —
        instrument the hit path next
      - [ ] Looper 21× slower on denormal input (warning): the FTZ guard is in place, so it is
        data-dependent work, not denormals — profile with the validator's input
      - [x] Morph: the saved `model` was the store as is, so an instance that had processed
        (factory table loaded for its Target) saved differently from one that had not; the
        state now saves the model the audio thread would use for the current Target
      - [x] scan-time warnings 0.3–2 s: measured 2026-10-21 as load noise, not plugin work — the
        warned plugin changes every full run (Vocoder, Wind, Chorus, Piano…), and the same
        bundles validated three at a time while the machine is idle scan clean every time
      - [x] Piano state-buffered-streams, 1 run in 3: `save()` read `requested_path` (taken by the
        loader) then `info.path` (filled once loaded) — `null` in between. `state_path` now
        holds the last request untouched by the loader; 5/5 clean
- [x] effect::stereo_block (events, dirty/gesture, bypass that feeds the scope) — EQ migrated;
      the other 30 plugins one batch at a time, each batch with its quality snapshots unchanged
      - [x] split into `begin_events` + `run_ports` (2026-10-21): a plugin that snapshots its
        params before the audio half ran a block behind on automation with the one-call form
      - [x] batch 1: Chorus, Harmonic, MidSide, Filter, Limiter (snapshots unchanged, validator
        clean; MidSide's denormal warning gone)
      - [x] batch 2 (side-chain, `effect::run_main_port`: key port captured first, port 0 only):
        Delay, Reverb, Supermass — `stereo_process` now takes slices, no own bypass/scope
      - [ ] batch 3 side-chain: Compressor, Multiband, Soothe, Morph, Double, Vocoder (same shape)
      - [ ] the rest (Saturator per-channel loop, LinEQ `begin_block` prelude + host callback,
        Vocal, Pitch, Tune, Stretch, Granular, Formant, Ambient, Looper, NAM, Spectrum, synths)
- [x] proptest invariants (synth-core/tests/invariants.rs) — found: NaN key froze key_factor,
      RBJ shelf with slope > 1 gives NaN coefficients; both guarded
- [x] bus meter controls (`sdsp-bus controls`, tools/sdsp-bus/src/controls.rs, 15 known-answer
      scenes; tests/controls.rs pins them). Fixed what they found: a partial `fader_db`/`prefer`
      leaked by containment ("Gtr" moved "Gtr DI"); `by_db` averaged silent frames (+3 dB on a
      half-time clash); `levels` carried buses; `tracks` was the last frame only; equal-level
      clashes now say `louder: ""`
- [x] mixopt (tools/mixopt.py, references/optimize.md): bounded Nelder–Mead over a whitelist of
      chain params against ref band shape / LUFS / masking / stem level, 0.14 s per evaluation
- [ ] Morph f0 (done by agent: autocorrelation voicing, −18 dB/oct profile roll-off, Breath only
      inside the comb, warm-up gate) — re-measure on the Kubyz Drone side-chain in the demo project
- [x] DSP review fixes (agent, 2026-10-21): Soothe key cut is a host-free `Engine` with the 24-band
      leak matrix solved exactly (Key Depth 12 reads −12.1…−12.6 at every Q; was −25.7 at Q 5);
      Multiband band-count change resets the old top band (−15.7 dB first block → 0), NaN/−120 key
      → no gain, damped Newton + continuation and a ½-octave minimum crossover (251 → 20 misses >1 dB
      on 2000 random targets, 0 at ½ oct); Compressor Dual peak stage runs in parallel on the raw
      peak, gr = min(rms, peak), Classic↔Dual seeds/clears the RMS state; Double Key Open with no
      key on the bus = gain 1.0 (was −151 dBFS); Saturator Inflate input gain = Drive − 6 dB (0
      flat-topped samples on a −3 dBFS tone, was 9351)

## Done
- [ ] GitHub release CI (fortunto2/superduper-dsp): the last three runs ended cancelled/failed —
      v0.15.0 (run 39547209693) uploaded 35 Windows assets, "macOS arm64" cancelled, release notes
      skipped; the two runs before it cancelled/failed too. Read the macOS job log (concurrency
      cancel vs timeout) before the next tag
