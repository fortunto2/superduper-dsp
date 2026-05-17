# Claude Code instructions — SuperDuper DSP

CLAP plugin platform in Rust. We ship **standalone effect plugins** (one .clap
per effect: SuperDuper Reverb, SuperDuper Supermass, …) built on a shared
infrastructure of DSP blocks, CLAP helpers, build versioning, and spectrum
analysis. The original "shell with hot-loaded dylibs" idea is shelved — REAPER
caches param layouts per (plugin_id, slot) which makes dynamic layouts
unworkable. Each effect = its own crate + its own CLAP id + fixed param table.

## Current state — vocal chain complete (8 plugins)

- **superduper-reverb** — Dattorro figure-of-eight plate. Sidechain ducking.
- **superduper-supermass** — Valhalla-style cascade (reverb 35m/15s →
  stereo chorus → reverb 50m/28s) on fundsp 0.23. Sidechain ducking.
- **superduper-spectrum** — pass-through analyzer (Spectrum / Spectrogram
  / Split view, 3 colour palettes).
- **superduper-saturator** — Tape / Tube / Soft-tanh curves + Tilt EQ.
- **superduper-delay** — 3rd-order Lagrange-interp delay, tape-style
  feedback saturation, ping-pong + slap modes, sidechain ducking.
- **superduper-compressor** — soft-knee feed-forward, peak+LP detector,
  2 ms lookahead, sidechain HPF, external sidechain port, live GR meter.
- **superduper-eq** — 3-band parametric (low shelf + mid peak + high shelf)
  RBJ biquad + HP/LP, output trim.
- **superduper-limiter** — lookahead brickwall, 4× true-peak detection
  on a sidechain upsampler, live GR meter.

All eight ship as `.clap` bundles with a `[bNNNNN]` build-number suffix
in their display name. Released for macOS arm64 + Windows x64 via CI.

`tools/sdsp-runner` is the standalone CLAP host — loads any `.clap`,
plays a WAV file through it to cpal output (`sdsp-runner <plugin.clap>
[<input.wav>]`). Useful for fast dev loop without REAPER.

Planned: SuperDuper Ambient (multi-track autonomous generator from
rust-synth), SuperDuper Pad (note-driven synth via MIDI input port).

## Workspace layout

```
superduper-dsp/
  sdk/                       lib utilities used by every plugin
    src/
      clap_helpers.rs        ParamDef, apply_param_events, split_io
      build_meta.rs          plugin_display_name!, version_string!, build_num!, build_date!
      dsp.rs                 OnePole, EnvelopeFollower, DcBlocker, soft_clip etc.
  sdk-build/                 build.rs helpers (one-line per-plugin build.rs)
    src/lib.rs               emit_build_meta()
  sdk-macros/                proc-macro params!{} (M2 planned)
  synth-core/                shared DSP — anything reusable across effects
    src/
      dsp_blocks.rs          Ducker, Tilt, DcBlocker, SmoothedParam
      analysis.rs            FFT, magnitude_spectrum_db, ascii_spectrum, sine sweep
      supermass.rs           Valhalla-style cascade reverb (Net builder)
    tests/dsp_blocks.rs      9 unit tests on shared blocks
  effects/
    superduper-reverb/       Dattorro plate effect plugin
    superduper-supermass/    Cascade reverb effect plugin
    example-passthrough/     toy effect for the hot-reload path
  plugin/                    old shell-plugin code (deprecated, kept for reference)
  daemon/, protocol/         IPC infrastructure (deprecated for now)
  scripts/
    build_reverb_bundle.sh
    build_supermass_bundle.sh
    restart_reaper.sh
```

## How to add a new effect plugin

Step-by-step. Copy SuperDuper Reverb or Supermass as a starting point.

### 1. Create the crate

```bash
mkdir -p effects/superduper-<name>/{src,tests}
```

Add to workspace `Cargo.toml`:
```toml
members = [..., "effects/superduper-<name>"]
```

### 2. `effects/superduper-<name>/Cargo.toml`

```toml
[package]
name = "superduper-<name>"
version = "0.1.0"
edition = "2021"
license = "MIT"
description = "..."

[lib]
name = "superduper_<name>"
crate-type = ["cdylib", "rlib"]

[dependencies]
clack-plugin = "0.1"
clack-extensions = { version = "0.1", features = ["params", "audio-ports", "gui", "clack-plugin", "raw-window-handle_05"] }
clack-common = "0.1"
atomic_float = "1"
parking_lot = "0.12"
superduper-dsp-sdk = { path = "../../sdk" }
superduper-synth-core = { path = "../../synth-core", features = ["gui"] }

# GUI stack — must match versions used in superduper-reverb / supermass.
egui = "0.33"
egui-baseview = { git = "https://github.com/BillyDM/egui-baseview" }
baseview = { git = "https://github.com/RustAudio/baseview.git", rev = "237d323c729f3aa99476ba3efa50129c5e86cad3" }
raw-window-handle = "0.5"

[build-dependencies]
superduper-dsp-sdk-build = { path = "../../sdk-build" }

[dev-dependencies]
clack-host = { version = "0.1", features = ["clack-plugin"] }
clack-extensions = { version = "0.1", features = ["params", "audio-ports", "log", "clack-host"] }
superduper-synth-core = { path = "../../synth-core" }
```

### 3. `build.rs` (one line)

```rust
fn main() { superduper_dsp_sdk_build::emit_build_meta(); }
```

This puts `SDSP_BUILD_NUM` / `SDSP_BUILD_DATE` env vars into the compile so
the plugin's display name shows `[bNNNNN]`.

### 4. `src/lib.rs` skeleton

Crib from `effects/superduper-reverb/src/lib.rs`. Key pieces:

```rust
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_num, build_date, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{Ducker, SmoothedParam, DcBlocker, Tilt};

const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Param1", min: 0.0, max: 1.0, default: 0.5, unit: "" },
    // ... fixed layout — never add/remove at runtime
];
const P_PARAM1: usize = 0;

pub struct PluginShared { pub params: [AtomicF32; PARAMS.len()], pub bypass: AtomicBool }
// ... use ParamDef::init_atomics in new()

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 { if is_input { 2 } else { 1 } }
    fn get(&mut self, index: u32, is_input: bool, w: &mut AudioPortInfoWriter) {
        match (index, is_input) {
            (0, _) => /* main I/O, IS_MAIN, in_place_pair: Some(ClapId::new(0)) */,
            (1, true) => /* Sidechain, no IS_MAIN, in_place_pair: None */,
            _ => {}
        }
    }
}

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 { PARAMS.len() as u32 }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info(PARAMS, idx, info);
    }
    fn value_to_text(&mut self, id, value, w) -> _ { ParamDef::write_display(PARAMS, id, value, w) }
    fn text_to_value(&mut self, id, text) -> _ { ParamDef::parse_text(PARAMS, id, text) }
    fn flush(&mut self, ev, _) { superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev); }
}

impl DefaultPluginFactory for ... {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.<name>",                      // stable CLAP id
            plugin_display_name!("SuperDuper <Name>"),    // → "SuperDuper <Name> [bNNNNN]"
        )
        .with_version(version_string!("0.1"))             // → "0.1.NNNNN (YYYY-MM-DD)"
        // ...
    }
}

clack_export_entry!(SinglePluginEntry<...>);
```

### 5. GUI (optional but recommended)

Every effect ships with an egui_baseview window. Three files per plugin
(`gui.rs` + `presets.rs` + GUI extension in `lib.rs`) following the same
pattern, all sitting on top of `superduper_synth_core::gui` shared helpers.

Minimum skeleton — see `effects/superduper-reverb/src/gui.rs` for the full
~120-line working example. The shape is:

```rust
// src/gui.rs
use superduper_synth_core::gui as core_gui;
use crate::presets::PRESETS;

pub const DEFAULT_WIDTH: u32 = 720;  pub const DEFAULT_HEIGHT: u32 = 760;
pub const MIN_WIDTH: u32 = 520;      pub const MIN_HEIGHT: u32 = 640;
pub const MAX_WIDTH: u32 = 1400;     pub const MAX_HEIGHT: u32 = 1200;

pub type ResizeBridge = core_gui::ResizeBridge;
pub fn new_resize_bridge() -> ResizeBridge {
    core_gui::new_resize_bridge(DEFAULT_WIDTH, DEFAULT_HEIGHT)
}

struct GuiState { shared: SharedParams, resize: ResizeBridge,
                  applied_size: (u32, u32), selected_preset: Option<usize>,
                  preset_names: Vec<&'static str> }

pub fn open_window<P: HasRawWindowHandle>(parent: &P, shared, resize) -> WindowHandle {
    EguiWindow::open_parented(parent, settings, GraphicsConfig::default(), state,
        |ctx, _, _| core_gui::install_default_style(ctx),
        |ctx, queue, state| {
            // Host-resize bridge
            let want = core_gui::read_bridge(&state.resize);
            if want != state.applied_size { queue.resize(...); ... }
            // Draw
            egui::CentralPanel::default().show(ctx, |ui| {
                if let Some(i) = core_gui::top_bar(ui, "<Plugin Name>", ..., bypass, "<combo_id>", &state.preset_names, &mut state.selected_preset) {
                    apply_preset(state, i);
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    core_gui::section(ui, "Section A", |ui| {
                        core_gui::param_row(ui, &state.shared.params[P_FOO], &PARAMS[P_FOO]);
                        // ...
                    });
                    core_gui::section(ui, "Section B", |ui| { /* ... */ });
                });
            });
            ctx.request_repaint();
        },
    )
}
```

The presets file (`src/presets.rs`) is just a static `&[Preset]` slice where
each `Preset` is `(name, [f32; PARAMS.len()])`. `Preset::from_overrides`
const-builds one from a sparse list of (index, value) — typo-proof and
falls back to the param table's default for unspecified slots.

Wiring CLAP GUI extension in `lib.rs`:

```rust
use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints,
    GuiSize, PluginGuiImpl, Window as ClapGuiWindow,
};

// In Plugin::declare_extensions:
builder
    .register::<PluginAudioPorts>()
    .register::<PluginParams>()
    .register::<clack_extensions::gui::PluginGui>();

// PluginMainThread fields:
gui_handle: Option<baseview::WindowHandle>,
gui_resize: gui::ResizeBridge,

impl PluginGuiImpl for PluginMainThread<'_> {
    // is_api_supported / get_preferred_api: COCOA/WIN32/X11, embedded only
    // can_resize: true; get_resize_hints: horizontal + vertical, no aspect ratio
    // adjust_size / set_size: clamp to MIN_*..MAX_* and write to gui_resize
    // set_parent: open_window(&window, shared.shared_handle(), gui_resize.clone())
    // destroy: gui_handle = None  (dropping it closes the egui window)
}
```

Important: `PluginShared.inner` must be `Arc<SharedParamsInner>` so the GUI
thread can clone it via `shared.shared_handle()`. See reverb's `lib.rs`
for the boilerplate including the `Deref` impl that keeps existing
`shared.params[i]` call sites compiling.

### 6. Tests

Every effect crate ships at least 3 test files:
- `tests/dsp_smoke.rs` — drives the DSP block directly, validates RMS / peak /
  stability / mono-to-stereo behavior. No CLAP, no fundsp Net wrapping. Fast.
- `tests/clap_e2e.rs` — uses `clack-host` to load the plugin in-process,
  activates, runs a buffer through `process()`, asserts output ≠ input.
  Catches CLAP plumbing bugs (audio-ports, param routing) without REAPER.
- `tests/spectrum.rs` — runs an impulse / noise burst / sine sweep through
  the DSP, FFTs the tail, prints ASCII spectrogram via `analysis::ascii_spectrum`.
  Read with `cargo test --test spectrum -- --nocapture` — the ASCII art
  is the assertion target (a human or AI sees the shape and decides if it's right).

### 6. Bundle script `scripts/build_<name>_bundle.sh`

Copy `scripts/build_reverb_bundle.sh`, change two strings: the package name
and the CFBundleIdentifier. The script also installs to
`~/Library/Audio/Plug-Ins/CLAP/<Name>.clap`.

## Shared building blocks — use these instead of rolling your own

**`superduper_synth_core::dsp_blocks`:**
- `Ducker` — peak-envelope-driven sidechain gain reducer with asymmetric
  attack/release. Same primitive both reverbs use.
- `Tilt` — single-shelf brightness control (±6 dB).
- `DcBlocker` — first-order HPF at ~38 Hz. **Put it before any feedback
  loop** (reverbs, delays, comb filters). DC drift accumulates and otherwise
  drowns the tail.
- `SmoothedParam` — one-pole interpolator for CLAP slew. Without it, dragging
  Mix or Width sends a step function into the audio and you hear zipper noise.
  Snap to host-loaded value at `activate()` time so the first block isn't a
  fade-in.

**`superduper_synth_core::analysis`:**
- `magnitude_spectrum_db(samples)` — Hann window + real-FFT → dB per bin.
- `spectrum_with_freq(samples, sr)` — same but pairs each bin with its Hz.
- `ascii_spectrum(spec, opts)` — render as ASCII bar chart. Use in tests
  with `-- --nocapture` so the chart prints.
- `frequency_response_sine_sweep(process_one, sr, freqs, secs)` — log-spaced
  sine sweep through a closure-shaped DSP block → measured gain curve.
- `log_freq_grid()` — standard 1/3-octave grid 20 Hz–20 kHz.

**`superduper_synth_core::supermass`:**
- `build_wet() -> fundsp::Net` — cascade reverb graph. Call
  `net.set_sample_rate(...)` in `activate()`. Net mutation is RT-unsafe so
  geometry stays fixed; expose Mix/Width/etc. as post-process knobs.

**`superduper_dsp_sdk::clap_helpers`:**
- `ParamDef` struct — declare your `const PARAMS: &[ParamDef]`. Methods
  `write_info`/`write_display`/`parse_text` plug directly into the CLAP
  param extension trait.
- `apply_param_events(params, events)` — reads `ParamValueEvent`s and stores
  into atomics. Critical: `pv.param_id()` returns `Option<ClapId>`, NOT
  `ClapId` — destructure or you silently lose all events.
- `split_io(ChannelPair)` — unifies InputOutput / InPlace / OutputOnly /
  InputOnly into `(read_slice, write_slice)`.

**`superduper_dsp_sdk` build-meta macros** (require `sdk-build` in build-deps):
- `plugin_display_name!("Base Name")` → `"Base Name [bNNNNN]"`
- `version_string!("0.X")` → `"0.X.NNNNN (YYYY-MM-DD)"`
- `build_num!()` / `build_date!()` for log lines

## Hard-won lessons — read before editing

1. **`audio-ports` extension is mandatory** in REAPER, even for plain
   stereo-in/stereo-out. Without it `process()` is called but no audio is
   routed through. Always register with `IS_MAIN` flag and
   `in_place_pair: Some(ClapId::new(0))`.

2. **`ParamValueEvent::param_id()` returns `Option<ClapId>`.** Compare via
   `let Some(id) = pv.param_id() else { continue }` — never directly to a
   `ClapId` (compiles, silently always false).

3. **CLAP crate versions are 0.1.x**, not 0.4.x. Don't trust the original
   scaffolding's pinned versions.

4. **`#![no_std]` breaks `f32::exp/tanh/powf`.** Keep std on.

5. **`~/.local/bin/python3.*` is Pyodide WASM**, not native CPython.
   Use `/opt/homebrew/bin/python3.13` for any native Python tooling.

6. **`notify::recommended_watcher()` blocks for FSEvents init on macOS.**
   Don't call it during plugin scan (`new_shared()`) — REAPER's scan path
   serialises everything and you'll get 5-second per-plugin timeouts.

7. **stderr from Dock-launched plugin hosts is dropped.** Write logs to
   `~/.superduper-dsp/<plugin>.log` directly (each plugin has its own
   `OnceLock<Mutex<File>>` + `slog!`/`rlog!` macro). Not RT-safe — gate
   behind a build flag for shipping.

8. **`CARGO_TARGET_DIR=/Users/rustam/.cargo-target`** is set globally on
   this machine. Bundle scripts respect `${CARGO_TARGET_DIR:-./target}`.

9. **fundsp Net is RT-unsafe to rebuild.** Build once in `activate()`,
   call `set_sample_rate()`, never rebuild. Use `AudioUnit::tick(in, out)`
   per sample (or `process()` per block).

10. **REAPER caches param layouts per (plugin_id, FX-slot)** in the project
    file. Changing param count or order breaks track-level settings. Standalone
    plugins with fixed layouts sidestep this; the original shell-plugin
    approach kept hitting it. Don't change `PARAMS` after shipping unless
    you bump the CLAP id (which forfeits user automation).

11. **`Vec` / `Box::new` / any heap alloc in `process()` = crash potential.**
    Pre-allocate scratch buffers at `activate(max_frames_count)` into
    `Box<[f32]>`. The sidechain snapshot pattern in reverb/supermass shows
    the right shape.

12. **DC blocker before any feedback loop.** Without it, accumulating DC
    eventually drowns the reverb tail in static hum. One-line cost, big win.

13. **Smooth user-facing params (Mix, Width, Drive).** Atomic-read per sample
    is fine, but the *target* changes in steps. Slew through SmoothedParam
    to kill zipper noise on knob drags.

## DSP code style rules — never violate inside `process()`

- No heap allocation (no `Vec`, `Box::new`, `String`, `HashMap`).
- No `Mutex` / `RwLock` — atomics only.
- No syscalls (no `println!`, no file I/O, no networking).
- No `panic!`, no `unwrap()`, no `expect()`, no `arr[i]` indexing that can
  out-of-bounds. Use `.get()` + `Option` or bound-checked `unsafe { *ptr.add(i) }`.
- `core::f32::*` math (`.sin()`, `.tanh()`, `.powf()`) is fine — we link std.
- `static mut` for module-level state is OK if wrapped in `unsafe` and
  documented as audio-thread-only.

## Workflow

### Build a specific plugin + install
```bash
./scripts/build_reverb_bundle.sh
./scripts/build_supermass_bundle.sh
# new effects: write scripts/build_<name>_bundle.sh
```

### Run all tests
```bash
cargo test --release -p superduper-reverb -p superduper-supermass -p superduper-synth-core
```

### See ASCII spectrum output
```bash
cargo test --release -p superduper-reverb --test spectrum -- --nocapture
cargo test --release -p superduper-supermass --test spectrum -- --nocapture
```

### Tail plugin debug logs (during REAPER session)
```bash
tail -F ~/.superduper-dsp/reverb.log
tail -F ~/.superduper-dsp/supermass.log
```

### Restart REAPER cleanly
```bash
./scripts/restart_reaper.sh        # graceful Cmd+Q + open
./scripts/restart_reaper.sh --force # SIGKILL + open
```

### Cutting a release

Tag-driven via GitHub Actions. Two paths:

**Local-only (macOS only):**
```bash
./scripts/build_release.sh 0.1.0
# produces dist/release-0.1.0/*.zip + SHA256SUMS
```
Upload those zips manually to a GitHub release if you prefer the manual route.

**Full release (macOS + Windows via CI):**
```bash
# Bump versions in Cargo.toml across all three plugin crates first.
git tag -a v0.1.0 -m "Release 0.1.0"
git push origin v0.1.0
```
GitHub Actions `.github/workflows/release.yml` runs:
1. `build-macos` on `macos-14` runner — produces arm64 .clap bundles, ad-hoc
   signs them, zips per-plugin + combined.
2. `build-windows` on `windows-latest` — produces .clap files (single .dll
   each), zips them.
3. `release-notes` — concatenates generated release notes + manual body
   with install instructions.

Tags must match `v*` (e.g. `v0.1.0`, `v0.2.0-rc1`). Manual dry-runs:
Actions → Release → "Run workflow" — uploads artifacts to the workflow run
instead of creating a GitHub Release.

User-facing install instructions live in `INSTALL.md` (auto-included in
each release zip).

### REAPER plugin cache problems
If a rebuild doesn't take effect: REAPER Preferences → Plug-ins → CLAP →
**Clear cache and re-scan**. Build numbers in the display name (`[bNNNNN]`)
let you tell which build is loaded without digging through Plugin Info.

## Sidechain routing in REAPER

Reverb and Supermass declare `Sidechain` as input port index 1 (no IS_MAIN
flag, type STEREO). To route something into it:
1. Right-click the plugin in the FX chain → **Pin Connector**.
2. The left half lists track channels (1-4 typical), the right half lists
   the plugin's pins (3-4 are the sidechain L/R).
3. Drag connections — e.g. track channels 3-4 → plugin pins 3-4. You'll need
   to enable 4-channel routing on the track first (right-click track →
   I/O → set output channels to 4).
4. Send another track's audio to channels 3-4 of the reverb track.

If no sidechain is routed, both ducker key signals fall back to dry input
(works on insert vocals out of the box).

## Distribution model — Stage C (decided 2026-05)

Every effect is its own `.clap` bundle with a unique CLAP id. Users get
plugin-by-plugin distribution: SuperDuperReverb.clap, SuperDuperSupermass.clap,
… separate files on disk, separate entries in FX browser, separate REAPER
project state. Old "shell + dynamic effects folder" idea is dead — it
fought REAPER's param cache and lost.

## When in doubt

Ask the user. Better one clarification question than three failed compiles.
