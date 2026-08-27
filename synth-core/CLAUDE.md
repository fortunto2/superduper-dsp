# synth-core — shared DSP library

Anything reusable across SuperDuper effect/synth plugins lives here.

## Reaching iOS (live2play in-app synth)

**synth-core is the bridge to iPhone.** The live2play app's in-app synth links a thin C-ABI
staticlib (`mobile/sdsp-ios`) that depends ONLY on synth-core with `default-features = false`
(no `gui`/egui/clack — those don't cross-compile to iOS). So **any DSP placed in synth-core
automatically reaches the phone**; DSP that lives in a plugin crate (`effects/superduper-*`) does
NOT, because those crates hard-depend on egui/baseview.

**To make a plugin's DSP playable on iOS:** move its pure-DSP module into synth-core (keep the
egui GUI in the plugin crate, re-export the moved module under its old path so the desktop plugin
keeps compiling — see `superduper-wave`'s `pub use superduper_synth_core::wave_osc as osc;`). Then
wire it into `mobile/sdsp-ios/src/lib.rs` and rebuild:

```
make ios        # rebuilds SDSP.xcframework into the reelcam repo (synth-core DSP → iPhone)
# then in ~/startups/active/reelcam:  make deploy
```

This is the repeatable "rebuild my DSP for iPhone" step. `wave_osc.rs` is the worked example.

## Modules

- **`dsp_blocks`** — `Ducker`, `Tilt`, `DcBlocker`, `SmoothedParam`, `Xorshift`
  (the shared RT-safe deterministic PRNG — use it instead of hand-rolling
  another xorshift; being a struct rather than a `&mut self` method also keeps
  field-disjoint borrows working). RT-safe
  building blocks. Default-constructible, process methods take params as
  arguments (no internal state besides what each block actually needs).
- **`analysis`** — FFT (`magnitude_spectrum_db`), ASCII spectrogram
  (`ascii_spectrum`), sine-sweep frequency response. For tests only — not
  RT-safe (uses heap, mutex-guarded planner cache).
- **`formant`** — 3-band parallel band-pass vocal-tract filter + the
  Peterson-Barney vowel table and the Bashkir/khomus presets. Used by Kubyz,
  Wind, and Formant.
- **`formant_fx`** — the whole formant-articulator engine (tracker + resonators
  + mouth trajectory + drive/mix) behind SuperDuper Formant. Lives here rather
  than in the plugin crate for the usual reason: DSP parked under `effects/`
  can't reach the mobile staticlib.
- **`formant_track`** — live F1/F2/F3 estimator (Hann FFT 1024/256 →
  pre-emphasis → frequency-proportional envelope smoothing → per-formant
  peak-pick → glide). Gates on the newest hop only so the estimate **freezes**
  instead of chasing the noise floor. Drives SuperDuper Formant's Follow mode.
- **`pitch`** — `detect_pitch_hz` (offline, Wave's WAV import),
  `epoch_sharpness` (the descriptor `pitch_engine` routes on — crest excess
  times lag-`t0` tonality, RT-safe, alloc-free, no FFT; its doc comment carries
  the measured threshold and the two cheaper descriptors that were rejected on
  measurement) and `YinPitchTracker`, the streaming YIN fundamental tracker behind Tune,
  Pitch (Voice/PSOLA), Vocoder, Harmonic and Wind. The tracker's difference
  function is **FFT-based** (d(τ) = ΣX²-prefix sums − 2·autocorr via one
  realfft pair over the zero-padded window) — the scalar O(τmax·W) version
  cost ~800k f64 mul-adds per analysis and made Tune (which runs TWO
  trackers: its own + the PSOLA engine's) burn 25% of a core; FFT brought
  it to ~5%. Keep new pitch consumers on this tracker instead of calling
  `yin_pitch_window` per hop — the scalar path stays only for offline use.
- **`swiftf0`** — SwiftF0 (lars76, CC-BY-4.0) neural pitch tracker as pure-Rust
  streaming inference: resample→16 kHz, STFT 1024/256, 5× Conv2d 5×5 + 1×1
  projection over 200 log bins, ±9-bin weighted decode with a confidence
  output. Weights are a 379 KB `include_bytes!` blob. Costs ~5% of a core
  (vs YIN's 0.5%) and lags one frame, and it earns that where YIN cannot:
  on noisy/breathy material YIN jumps a full octave in 100% of frames while
  SwiftF0 stays within 1.3 cents (`tools/pitch-bench` prints the table). The
  conv kernel is tiled 12 frequencies at a time — the naive axpy form was
  memory-bound at 1.4× realtime, tiling made it 15×. Streaming zeroes the
  two future time taps (causal), which costs ~6 cents against the offline
  model; matching it exactly would need 160 ms of lookahead. Selected in
  superduper-tune by the `Model` param. Core ML variant + conversion recipe:
  `~/Music/1music/swiftf0-coreml/`.
- **`pvoc`** — STFT phase vocoder (smbPitchShift-style: true-frequency per bin,
  bins moved to `k·α`, phase re-accumulated, iFFT + OLA). Moved here from
  `effects/superduper-pitch` so **Tune** and the iOS staticlib can reach it —
  an effect crate depending on another effect crate would have been a new and
  wrong direction. The compatibility re-export under `superduper_pitch::pvoc`
  has since been removed along with that crate's `dsp` shim — everything now
  names `synth_core::pvoc` directly. Handles polyphony, and is the fallback
  whenever PSOLA's assumptions do not hold.
- **`pitch_engine`** — owns a `PitchShifter` and a `PhaseVocoder` and decides
  which one runs, so Pitch and Tune cannot drift apart. `Mode::Auto` measures
  `pitch::epoch_sharpness` on the period PSOLA is already tracking and routes
  at a threshold of 0.9; six agreeing readings switch it, one does not.
  Three design points that are load-bearing rather than incidental:
  * **latency is the max of both engines, fixed at construction** — hosts
    mishandle PDC that moves, and padding both to the same number is also what
    makes their outputs sample-aligned, so the crossfade mixes two versions of
    the same moment;
  * **in Auto both engines run every block** — a cold engine emits nothing for
    its whole latency, so fading one in from cold is a fade into silence. That
    is the CPU price of an inaudible switch. Forced modes run one engine and
    warm the incoming one at zero gain for its latency *plus one STFT window*
    first (warming for only the latency left a measured 1.9 dB dip);
  * **equal-power, not linear** — measured, not assumed: the two renderings
    correlate at r = 0.13 on a pulsed source, so they sum in power.
  The PSOLA floor is 70 Hz here rather than the engine's 95 Hz default, which
  is what finally locks an 87 Hz voice, at 42 → 57 ms of latency.
  The descriptor is measured in EVERY mode, not just Auto: besides picking the
  engine it gates PSOLA's per-grain epoch snap, and that matters most exactly
  where routing is switched off (forced Voice on a synth pad: +2.7 → −33.4 dB).
  `tracked_hz()` exposes the engine's YIN estimate so a caller does not run a
  second identical tracker (superduper-tune used to). `reset()` clears both
  engines, and an oversized block is chunked rather than indexing past scratch.
- **`melody`** — offline note model: cut a pitch curve into `Note`s (unvoiced
  gaps, held pitch jumps, minimum duration), pick a target per note, and emit
  a per-frame shift curve. The point is that a note is corrected **as one
  object** — its median moves to the target, the shape inside it (vibrato,
  scoop) is untouched — which is what separates Melodyne-style editing from
  live autotune. Allocates and needs the whole take, so it is offline-only.
  Drives `tools/sdsp-tune`; a standalone editor would sit on the same model.
- **`spectral`** — `StftProcessor` (streaming STFT overlap-add with a per-frame
  callback; one shared `hop`, so an algorithm needing different analysis and
  synthesis hops can't use it) plus `smooth_proportional`, the
  frequency-proportional magnitude smoother shared by the formant tracker and
  the stretcher.
- **`granular`** — real-time granular cloud: capture ring + fixed grain pool,
  per-grain pitch/pan/direction/window, Freeze, and a DC-blocked feedback path.
  Level-compensated by √overlap. Drives SuperDuper Granular.
- **`paulstretch`** — extreme time-stretch: long-window STFT with randomised
  phase (blendable back toward the analysed phase), analysis hop = synthesis hop
  / stretch, Live and Freeze read-head policies. FFT plans for every selectable
  window size are pre-built so `Window` changes allocate nothing. Drives
  SuperDuper Stretch.
- **`supermass`** — Valhalla-style cascade reverb as a fundsp `Net`. Ported
  from rust-synth's `preset.rs`. Caller owns the Net: `set_sample_rate`,
  `tick(in, out)` per sample.
- **`gui`** (feature `gui`, gated) — shared egui_baseview helpers for every
  effect plugin's UI: `ResizeBridge`, `install_default_style`, `section`,
  `param_row`, `preset_combo`, `top_bar`. Pulls in `egui` and
  `atomic_float` only when feature is enabled.

## Adding a new shared block

1. New module file under `src/`, or extend `dsp_blocks.rs` if it's a small
   primitive.
2. `#[derive(Default)]` if possible. Public state is fine if it's documented
   as RT-safe.
3. The `process` method must:
   - Take `&mut self` plus runtime params (sr, gains, times).
   - Never allocate, never use `Mutex` / `RwLock`, never panic.
   - Return one sample (or a stereo pair) — block-rate processing is the
     caller's job.
4. Add tests to `tests/dsp_blocks.rs`. Aim for one positive and one negative
   case (does the thing / doesn't do anything stupid at edges).

## Adding a new fundsp graph

Like `supermass::build_wet()`:
- One function that returns `Net` — no parameters that change Net geometry.
- All runtime-tweakable knobs are applied by the caller as post-process
  (the reverb plugins do mix/width/drive/tilt outside the Net).
- Document the topology in the file header with an ASCII diagram.

## Adding analysis helpers

`analysis.rs` is for *test instrumentation*, not RT code. Use anything you
like (rustfft, image, plotters, etc.) but keep API simple:
- Input: `&[f32]` (mono) or `&[(f32, f32)]` (stereo).
- Output: `String` (for ASCII), `Vec<(f32, f32)>` (for raw data), or
  `Result<(), io::Error>` (for file dumps to /tmp).

## Adding GUI helpers

The `gui` feature gives every effect plugin the same look without copy-paste.
The pattern is:

1. In your plugin's `Cargo.toml`, enable the feature:
   ```toml
   superduper-synth-core = { path = "../../synth-core", features = ["gui"] }
   ```
2. Use `core_gui::install_default_style` in the egui-baseview `build` closure.
3. Use `core_gui::section` + `core_gui::param_row` for layout.
4. Use `core_gui::top_bar` for title + build label + preset dropdown + bypass.
5. Use `core_gui::new_resize_bridge` / `read_bridge` / `write_bridge` for
   host-driven resize.

Adding a new helper to `gui.rs`:
- Pure rendering function (no state) — fine to add directly.
- Stateful widget — return some `Response`-like struct so the caller can
  react to events, don't bake business logic into the helper.
- Pull `superduper-dsp-sdk` types (like `ParamDef`) only via the existing
  optional `superduper-dsp-sdk` dep — keep the feature graph clean.

## Tests

`tests/dsp_blocks.rs` — unit tests, run with
`cargo test --release -p superduper-synth-core`. New blocks: extend this
file rather than adding new test files; the suite is intentionally small
and fast. `tests/common/mod.rs` holds the shared reference signals (smooth
tone, pulsed voice, Rosenberg-pulse voice with adjustable breath) plus
`noise_to_harmonic` / `rms_db` / `max_step`; they mirror
`superduper-pitch/tests/engine_transparency.rs`, where the dB baseline lives,
so **keep the two in sync** — a sharpness number from here is only meaningful
paired with a dB number from there.

**Rule for any pitch-shifting engine** (learned the expensive way, lesson 24 in
the parent CLAUDE.md): test "do nothing" on BOTH a pulsed and a smooth source
before testing "do the thing", and measure the OUTPUT rather than what the
engine decided. TD-PSOLA was transparent at unity shift on a voice and turned
a −66.9 dB noise floor into −2.6 dB on a synth tone, and the autotune test
suite stayed green throughout because it only ever asked what correction the
corrector had chosen.
