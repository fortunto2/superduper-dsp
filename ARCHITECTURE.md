# SuperDuper DSP — architecture (single DSP core)

One **DSP core**, many thin **adapters**. The same Rust DSP powers the desktop CLAP plugins, their
VST3/AU wrappers, AND the iOS app (live2play). DDD: the *core* is the domain (pure signal
processing); everything platform- or host-specific is an *adapter* around it.

```
                ┌─────────────────────────── synth-core (THE CORE) ───────────────────────────┐
                │ pure DSP — NO gui, NO clack, NO platform deps. Cross-compiles to             │
                │ x86_64/aarch64 desktop AND aarch64-apple-ios.                                │
                │   dsp_blocks  (PadVoice, Biquad, SvfFilter, DelayLine, ADSR, saturation…)    │
                │   supermass   (Valhalla-style reverb Net)        wave_osc   (Wave synth)     │
                │   drum_voices (6 analog drums)                   kubyz      (jaw-harp model) │
                │   analysis · loudness · linphase · nam · formant · pitch · wav · user_preset │
                │   gui (egui helpers) — feature = "gui", OFF for iOS                          │
                └───────▲───────────────────────────▲───────────────────────────▲─────────────┘
                        │ rlib (gui)                 │ rlib (gui)                 │ default-features=false
        ┌───────────────┴───────┐       ┌───────────┴────────────┐    ┌──────────┴──────────────┐
        │ effects/superduper-*  │       │ tools/clap-wrapper     │    │ mobile/sdsp-ios         │
        │ CLAP plugin crates    │       │ → VST3 + AU (.clap     │    │ C-ABI staticlib →       │
        │ = core DSP + CLAP API │       │   loaded at runtime)   │    │ SDSP.xcframework        │
        │ + egui GUI            │       │                        │    │ (the live2play synth)   │
        └───────────────────────┘       └────────────────────────┘    └─────────────────────────┘
              DESKTOP (mac/win)               DESKTOP (any host)              iOS (live2play)
```

## The rule

- **DSP that should run everywhere lives in `synth-core`.** Instrument/effect engines, voices,
  filters, oscillators — all platform-free, no `gui`/`clack`/`egui`/filesystem in the hot path.
- **A plugin crate (`effects/superduper-<name>`) is a thin adapter:** core DSP + the CLAP param/
  audio/note plumbing + the egui GUI. It owns NO reusable DSP — it re-exports the core's.
- **`mobile/sdsp-ios` is a thin adapter:** depends on `synth-core` with `default-features = false`
  (no gui/egui — those don't cross-compile to iOS), exposes a C ABI, links into the app statically.
- **VST3/AU** need no DSP of their own — `clap-wrapper` loads the installed `.clap` at runtime.

## Extraction pattern (how a plugin's DSP joins the core)

Worked examples: `wave_osc` (Wave), `drum_voices` (Drum), `kubyz` (Kubyz). To prepare a plugin for
iOS / the single core:

1. **Move** the pure-DSP module(s) from `effects/superduper-<name>/src/` into `synth-core/src/`
   (a file, or a folder module for multi-file engines like `kubyz/`). Keep the GUI in the plugin.
2. **Fix imports** inside the moved files: `superduper_synth_core::X` → `crate::X`; any
   `crate::<sibling>` that also moved → `super::` / `crate::<new module>`. Constants shared with the
   plugin's presets (e.g. Kubyz `N_HARMONICS`) get ONE home in the core; the plugin re-exports them.
3. **Re-export from the plugin** under the old path so nothing else changes:
   `pub use superduper_synth_core::<module> as <oldname>;` (replaces `pub mod <oldname>;`). The old
   `src/<oldname>.rs` becomes dead — leave it in place, delete only on request.
4. **Verify** (all without a device):
   - `cargo build -p superduper-<name>` (desktop plugin still builds),
   - `cargo build -p sdsp-ios --release --target aarch64-apple-ios` (cross-compiles),
   - `cargo test -p superduper-synth-core` (DSP integrity),
   - then `make ios` ships it to live2play.

## Status — what's in the core vs still plugin-local

| Plugin | DSP location | iOS-ready |
|---|---|---|
| Pad / Ambient | `dsp_blocks::PadVoice` (always shared) | ✅ |
| Wave | `wave_osc` | ✅ |
| Drum | `drum_voices` | ✅ |
| Kubyz | `kubyz::{voice,trajectory}` | ✅ |
| Sampler | `voice.rs` reusable, but `bank.rs` scans the **filesystem** (desktop dirs) — needs an iOS
  sample-source adapter before extracting | ⚠️ deferred |
| Effects (Reverb/Filter/Delay/Saturator/EQ/Comp/…) | already in `dsp_blocks` + `supermass` + `loudness` + `linphase` | ✅ |

See `synth-core/CLAUDE.md` → "Reaching iOS" for the `make ios` step.
