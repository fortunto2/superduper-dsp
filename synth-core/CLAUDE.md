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

`tests/dsp_blocks.rs` — 9 unit tests, run with
`cargo test --release -p superduper-synth-core`. New blocks: extend this
file rather than adding new test files; the suite is intentionally small
and fast.
