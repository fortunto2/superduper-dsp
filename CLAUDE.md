# Claude Code instructions — SuperDuper DSP

CLAP plugin platform in Rust. We ship **standalone effect plugins** (one .clap
per effect: SuperDuper Reverb, SuperDuper Supermass, …) built on a shared
infrastructure of DSP blocks, CLAP helpers, build versioning, and spectrum
analysis. The original "shell with hot-loaded dylibs" idea is shelved — REAPER
caches param layouts per (plugin_id, slot) which makes dynamic layouts
unworkable. Each effect = its own crate + its own CLAP id + fixed param table.

## Current state — 13 plugins (9 effects + 4 instruments)

**Effects (audio-in, audio-out):**

- **superduper-reverb** — Dattorro figure-of-eight plate. Sidechain ducking.
- **superduper-supermass** — Valhalla-style cascade (reverb 35m/15s →
  stereo chorus → reverb 50m/28s) on fundsp 0.23. Sidechain ducking.
- **superduper-spectrum** — pass-through analyzer (Spectrum / Spectrogram
  / Split view, 3 colour palettes).
- **superduper-saturator** — Tape / Tube / Soft-tanh curves + Tilt EQ,
  2×/4× polyphase oversampling.
- **superduper-delay** — 3rd-order Lagrange-interp delay, tape-style
  feedback saturation, ping-pong + slap modes, sidechain ducking.
- **superduper-compressor** — soft-knee feed-forward, peak+LP detector,
  2 ms lookahead, sidechain HPF, external sidechain port, live GR meter,
  Clean/Pump/Smooth curves, Range + Hold params, oversampled ceiling clipper.
- **superduper-eq** — 3-band parametric (low shelf + mid peak + high shelf)
  RBJ biquad + HP/LP, output trim.
- **superduper-limiter** — lookahead brickwall, 4× true-peak detection
  on a sidechain upsampler, live GR meter.
- **superduper-vocal** *(new)* — split-band de-esser + mouth de-clicker
  tuned for rap vocals. 11th plugin.

**Instruments (MIDI-in or generator, audio-out):**

- **superduper-ambient** — autonomous chord-drone generator (no MIDI input).
- **superduper-pad** — polyphonic MIDI synth, 8-voice PadVoice pool +
  per-voice ADSR + TPT/ZDF SVF, click-free voice steal.
- **superduper-wave** *(new)* — wavetable bass/lead synth with
  mouse-editable curve (sharp/smooth nodes + Catmull-Rom, RDP simplify),
  mip-mapped anti-aliasing pyramid, per-voice unison + sub + noise +
  filter envelope + LFO with 3 destinations + tempo-sync LFO + Undo/Redo
  for the curve editor.
- **superduper-kubyz** *(new)* — physical-model jaw-harp / khomus.
  16-harmonic additive engine + 3-band bandpass formant + interactive
  IPA vowel pad + animated mouth trajectory (Circle / Sine / Figure-8 /
  Triangle / Line) + stereo motion from the trajectory + tempo-sync
  Mouth Rate + Tongue Pitch + Bashkir / Khomus / Real-D2 presets.

All thirteen ship as `.clap` bundles with a `[bNNNNN]` build-number suffix
in their display name. Released for macOS arm64 + Windows x64 via CI.

**Cross-cutting features now in every plugin:**

- **Automation write** — GUI knob moves emit `ParamValueEvent` to the
  host on the next `process()` block so REAPER / Bitwig / Studio One
  record them into FX automation lanes. Driven by a
  `dirty_params: [AtomicBool; PARAMS.len()]` array; GUI marks dirty,
  audio thread flushes via `sdk::clap_helpers::emit_dirty_param_events`.
- **CLAP state extension** — params + bypass round-trip through
  `Save FX chain preset…` and project save. Wave/Kubyz additionally
  persist their custom data (Wave: drawn frame_a curve; Kubyz: 16
  harmonic amplitudes + formant_bw / formant_gain). JSON-versioned
  via `sdk::clap_helpers::save_simple_state` / `load_simple_state`.
- **A/B compare + Initialize** — every plugin has the standard 4-button
  bar (A / B / copy → / init) under the preset combo via
  `core_gui::ab_init_bar` + `AbSnapshot`.
- **Live spectrum strip** — log-Hz × dB magnitude plot under the A/B
  bar. Backed by a lock-free `core_gui::LiveScope` ring buffer that
  audio thread pushes into and the GUI samples ~60 Hz for FFT.
- **User file presets** (Kubyz — pattern available for porting) —
  Save/Load buttons → `~/.superduper-dsp/<plugin>/presets/*.json`,
  plain-text and shareable.

**Synth-specific now in Pad / Wave / Kubyz:**

- **MIDI Pitch Bend** (status 0xE0). 14-bit value, 8192 center, scaled
  by per-plugin `Bend Range` param (default 2 ST).
- **MIDI CC mapping** — expressive control without an automation lane.
  CC writes go directly into param atomics without raising the dirty
  flag so the plugin doesn't echo CC back to the FX envelope and cause
  a feedback loop.
  - Pad: CC 1 / 11 / 71 / 74 → Modulation / Drive / Resonance / Cutoff (log)
  - Wave: CC 1 / 11 / 71 / 74 → LFO Depth / Cutoff / Resonance / WT Pos
    plus channel Aftertouch → LFO Depth.
  - Kubyz: CC 1 / 2 / 11 / 71 / 74 → Mouth Depth / Mouth Stereo /
    F1 / F3 / F2.

**Tempo sync** in Kubyz Mouth Rate + Wave LFO Rate — toggle `Sync`,
pick a `Div` (1/1 ↔ 1/16t, dotted + triplet variants), rate computes
from host BPM read out of `CoreEventSpace::Transport` events.

`tools/sdsp-runner` is the standalone CLAP host — loads any `.clap`,
plays a WAV file through it to cpal output (`sdsp-runner <plugin.clap>
[<input.wav>]`). Useful for fast dev loop without REAPER. **Effects only**
— synth/MIDI plugins (Pad) won't make sound through sdsp-runner because
it doesn't generate MIDI events; use the `clap_midi.rs` / `click_audit.rs`
test pattern instead (drives MIDI via clack-host and writes the result
to a WAV under `/tmp/`).

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
      dsp_blocks.rs          Ducker, Tilt, DcBlocker, SmoothedParam, Biquad,
                             EnvelopeDetector, compressor_gain_db (+ curves),
                             Oversampler2x, DelayLine, SlewLimiter2Pole,
                             OnePoleLp, PadVoice + PadParams, AdsrEnvelope +
                             AdsrParams, midi_note_to_hz, saturation primitives
                             (tanh_drive, tape_clip, tube_clip)
      analysis.rs            FFT, magnitude_spectrum_db, ascii_spectrum,
                             sine sweep, measure_thd_db, measure_aliasing_db,
                             measure_imd_smpte_db, make_bin_aligned_sine
      supermass.rs           Valhalla-style cascade reverb (Net builder)
      gui.rs                 shared egui_baseview helpers (feature = "gui")
    tests/dsp_blocks.rs      unit tests on shared blocks
  effects/
    superduper-reverb/       Dattorro plate effect plugin
    superduper-supermass/    Cascade reverb effect plugin
    superduper-spectrum/     pass-through analyzer + visualiser
    superduper-saturator/    tape/tube/soft-tanh + oversampling
    superduper-delay/        Lagrange-interp delay + tape feedback
    superduper-compressor/   feed-forward comp + lookahead + GR meter
    superduper-eq/           3-band RBJ parametric + HP/LP
    superduper-limiter/      lookahead brickwall + true-peak detect
    superduper-vocal/        split-band de-esser + de-clicker (rap vocal)
    superduper-ambient/      autonomous chord-drone generator
    superduper-pad/          polyphonic MIDI pad synth
    superduper-wave/         wavetable bass/lead synth (curve editor + mip-AA)
    superduper-kubyz/        physical-model jaw-harp / khomus
    example-passthrough/     toy effect for the (deprecated) hot-reload path
  tools/kubyz_analyser/      Python FFT analyser → Rust preset snippet
  tools/sdsp-runner/         standalone CLAP host (file-in → cpal-out)
  plugin/                    old shell-plugin code (deprecated, kept for reference)
  daemon/, protocol/         IPC infrastructure (deprecated for now)
  scripts/
    build_reverb_bundle.sh, build_supermass_bundle.sh, build_spectrum_bundle.sh,
    build_saturator_bundle.sh, build_delay_bundle.sh, build_compressor_bundle.sh,
    build_eq_bundle.sh, build_limiter_bundle.sh, build_vocal_bundle.sh,
    build_ambient_bundle.sh, build_pad_bundle.sh,
    build_bundle.sh             generic helper used by the per-effect scripts
    build_release.sh            full local release zip with SHA256SUMS
    restart_reaper.sh           graceful (or --force) REAPER restart
    install_local.sh, load_effect.sh, load_named_effect.sh
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
- `Biquad` — RBJ EQ Cookbook biquad (peaking, low/high shelf, HPF, LPF) in
  Direct Form II Transposed. Used by the EQ + the Vocal de-esser.
- `EnvelopeDetector` — asymmetric one-pole peak follower (attack ≠ release).
  Drives the compressor + limiter detection paths.
- `compressor_gain_db` + `compressor_gain_db_curve` with `CompressorCurve`
  (`Clean`/`Pump`/`Smooth`) — Giannoulis-Massberg-Reiss soft knee +
  alternative knee shapes. Use `Curve::Clean` for transparent SSL-style,
  `Pump` for FET 1176 punch, `Smooth` for sustained material.
- `Oversampler2x` + `oversample_apply` — 11-tap halfband FIR upsampler/
  downsampler. `os_mode` 0 = native, 1 = 2×, 2 = 4× cascaded. Wraps any
  per-sample non-linearity to keep aliasing below ~-80 dB.
- `DelayLine` — variable-length delay with 3rd-order Lagrange interpolation.
  Don't use linear interp for delay-tap fractional reads (6 dB high-shelf
  artefact at Nyquist).
- `SlewLimiter2Pole` — two cascaded one-poles, C¹ continuous. Use it for
  delay-time / pitch automation; a single one-pole has a discontinuous
  derivative on target changes and audibly clicks.
- `OnePoleLp` — simple one-pole LPF. The tone control inside a delay's
  feedback loop (every repeat gets darker).
- `PadVoice` + `PadParams` — autonomous 4-partial pad oscillator with built-in
  TPT/ZDF SVF lowpass + tanh saturation. Used by Pad (note-driven) and
  Ambient (autonomous drone). **Don't reset the voice's filter state**
  between notes — preserving `lp_z1`/`lp_z2` avoids clicks on voice steal.
- `AdsrEnvelope` + `AdsrParams` — linear-attack / exp-decay+release ADSR
  with an explicit `AdsrStage` state machine. `gate_on()` resumes from the
  current level (no glitch on re-trigger during decay/release).
- `midi_note_to_hz` — `440·2^((n-69)/12)`. Use this, not a manual table.
- `tanh_drive`, `tape_clip`, `tube_clip` — three saturation curves with
  matched dB gain. Caller picks the flavour; `tube_clip` carries a tiny
  DC bias so always pair it with `DcBlocker` downstream.

**`superduper_synth_core::analysis`:**
- `magnitude_spectrum_db(samples)` — Hann window + real-FFT → dB per bin.
- `spectrum_with_freq(samples, sr)` — same but pairs each bin with its Hz.
- `ascii_spectrum(spec, opts)` — render as ASCII bar chart. Use in tests
  with `-- --nocapture` so the chart prints.
- `frequency_response_sine_sweep(process_one, sr, freqs, secs)` — log-spaced
  sine sweep through a closure-shaped DSP block → measured gain curve.
- `log_freq_grid()` — standard 1/3-octave grid 20 Hz–20 kHz.
- `measure_thd_db(samples, f0, sr)` — total harmonic distortion against
  the dominant peak (2nd through 8th harmonic). Feed an integer-cycle sine.
- `measure_aliasing_db(samples, f0, sr)` — max non-harmonic peak. For
  saturator tests, pick `f0 ≈ 0.45·sr` so harmonics all alias back into band.
- `measure_imd_smpte_db(samples, sr)` + `imd_smpte_input(n, sr)` — SMPTE
  IMD test (60 Hz + 7 kHz at 4:1) for tube/tape colour characterisation.
- `make_bin_aligned_sine(fft_len, sr, hz, amp)` — sine that lands exactly
  on an FFT bin so THD measurements don't leak the fundamental into its
  neighbours. **Use this for every spectrum-based assertion.**

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

14. **MIDI/note ports — declare BOTH dialects.** A synth plugin must register
    the `note-ports` extension with `NoteDialects::CLAP | NoteDialects::MIDI`
    and `preferred_dialect: Some(NoteDialect::Clap)`. Hosts pick whichever
    they speak — REAPER routes MIDI items as MIDI 1.0, some other hosts
    only send CLAP notes. Without both dialects, half of hosts silently
    drop your NoteOn events. Handle `CoreEventSpace::NoteOn / NoteOff /
    NoteChoke / Midi` in your event loop; treat the raw MIDI status nibble
    (0x90/0x80/0xB0 + CC123/CC120) for the MIDI dialect path.

15. **`OutputOnly` audio buffers — handle them.** REAPER (and others) hand
    instrument plugins a `ChannelPair::OutputOnly` buffer with NO input
    slice. Naive `split_io` patterns from effect plugins return `None` for
    OutputOnly and silently produce zero audio. Use
    `clap_helpers::output_slice` (added during the Pad/Ambient fix in
    commit 16d5148) which unwraps OutputOnly correctly. Symptom: synth
    is dead silent in the DAW but its tests pass.

16. **TPT/ZDF SVF over Chamberlin for any voice filter.** Chamberlin SVF
    blows up at cutoff > sr/6 (numerical instability from forward Euler
    integration). The Trapezoidal Zero-Delay-Feedback form (Zavalishin
    "Art of VA Filter Design", chap. 5) is unconditionally stable up to
    Nyquist and costs the same per sample. The PadVoice ported from
    rust-synth originally used Chamberlin and clicked at high cutoff —
    commit bdda936 switched it to TPT/ZDF.

17. **Voice steal — preserve oscillator + filter state, NOT envelope.**
    The instinct is to `Voice::default()` a stolen voice slot. That zeros
    SVF integrators (`lp_z1`/`lp_z2`) and oscillator phases, producing
    an audible click on every steal. Correct pattern: only assign the new
    `key`/`note_id`/`velocity`/`age_stamp` and call `env.gate_on()` to
    re-trigger the attack from whatever level the envelope currently has.
    For re-using a fully-idle slot, you can also keep the filter state —
    its `lp_z*` are already ≈ 0 from the release floor.

18. **Drag-knob "vanishing audio" is usually a too-fast slew.** With a 5 ms
    SmoothedParam time constant, dragging Cutoff from 16 kHz to 80 Hz
    spans 12-orders-of-magnitude on a log scale at one constant linear
    rate — the audible result is "the sound disappears for a moment".
    Either lengthen the time constant (30-50 ms is musical) or, better,
    parameterise the slew rate in *octaves per second* and convert to
    linear inside the SmoothedParam step. Don't fight the user's ear.

19. **Sample-discontinuity audit pattern.** When the user reports clicks/
    crackle but the bug is non-obvious, write a `tests/click_audit.rs`
    that drives the plugin through a realistic MIDI sequence via
    clack-host, records to `/tmp/<plugin>_click_audit.wav`, and asserts
    `max |x[n+1] - x[n]| < 0.4`. The WAV doubles as a listening test;
    the histogram + top-10-spike timing pinpoints the moment if anything
    fails. Pad uses this pattern (`effects/superduper-pad/tests/click_audit.rs`).

20. **CLAP latency reporting matters for PDC.** Compressor + Limiter add
    1-2 ms of lookahead; without `latency` extension implementation the
    DAW won't compensate and your plugin throws the parallel bus out of
    phase. Commit 54fdae7 added it — copy that pattern for any plugin
    with internal pre-delay.

21a. **CLAP automation write requires `ParamValueEvent` emit.** GUI
    knob moves just writing into `AtomicF32::store` don't reach the host
    — REAPER won't record into its FX envelope without explicit events
    coming back out of `process()`. Pattern: per-param `dirty: AtomicBool`
    raised by the GUI, audio thread runs
    `emit_dirty_param_events(&shared.params, &shared.dirty_params,
    events.output)` once at the top of every `process()`.

21b. **MIDI CC handlers must NOT raise the dirty bit.** Otherwise the
    plugin re-emits every incoming CC as a ParamValueEvent, REAPER
    re-records it into the FX envelope, and on the next playback the
    envelope replays into the CC handler again — runaway feedback. CC
    moves belong in the MIDI clip; only GUI-driven param changes
    belong in the FX envelope.

21c. **CLAP state extension is mandatory for any custom data.** Without
    `PluginStateImpl`, REAPER (and every other DAW) drops everything
    that isn't a CLAP param: harmonic bars, drawn curves, formant
    bandwidths. Symptom — user designs a patch, saves the project,
    reopens, and the visualised stuff is gone but the sliders sit at
    the saved values. The fix is two methods + a JSON struct.

21d. **`apply_preset` must mark every param dirty.** Otherwise picking
    a preset moves the knobs in the GUI but the host's automation lane
    only sees the old values, so a recorded preset switch silently
    reverts on playback. After updating every `params[i].store(...)`
    the helper must also `dirty_params[i].store(true, ...)`.

21e. **Host BPM is in the Transport event, not the Process struct.**
    Catch `CoreEventSpace::Transport(t)` inside the same event-walk
    loop as ParamValue / NoteOn / Midi; cache the tempo in a shared
    `AtomicF32 host_bpm`. Helper for division → Hz lives in
    `synth_core::dsp_blocks::sync_division_hz`.

21f. **One scope buffer per plugin = atomic per-slot ring buffer.**
    `core_gui::LiveScope` is lock-free (`AtomicF32` per slot,
    `AtomicUsize` head). Audio thread `push`es per sample, GUI
    `snapshot`s the last N for the spectrum strip. Inconsistency
    under contention shows as a slight wiggle — fine for a meter,
    much cheaper than a Mutex.

22. **per-plugin quality_audit tests.** Compressor/Saturator/EQ ship a
    `tests/quality_audit.rs` that runs sine-sweep + THD + aliasing
    measurements and asserts numbers (e.g. "saturator at drive 0.5 has
    THD < -35 dB at 1 kHz, aliasing < -55 dB at 18 kHz under 4× OS").
    Use these as the basis for new plugins — the measurement primitives
    in `analysis.rs` (THD/IMD/aliasing) exist specifically for this.

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
./scripts/build_spectrum_bundle.sh
./scripts/build_saturator_bundle.sh
./scripts/build_delay_bundle.sh
./scripts/build_compressor_bundle.sh
./scripts/build_eq_bundle.sh
./scripts/build_limiter_bundle.sh
./scripts/build_vocal_bundle.sh
./scripts/build_ambient_bundle.sh
./scripts/build_pad_bundle.sh
# new effects: copy one of the above and change two strings (package name +
# CFBundleIdentifier). Or call ./scripts/build_bundle.sh <name> directly.
```

### Run all tests across the workspace
```bash
cargo test --release --workspace
# or one plugin at a time:
cargo test --release -p superduper-pad
```

### See ASCII spectrum / measurement output
```bash
cargo test --release -p superduper-reverb --test spectrum -- --nocapture
cargo test --release -p superduper-supermass --test spectrum -- --nocapture
cargo test --release -p superduper-saturator --test quality_audit -- --nocapture
cargo test --release -p superduper-compressor --test quality_audit -- --nocapture
# Pad click audit (writes /tmp/pad_click_audit.wav and prints histogram + tail spectrum):
cargo test --release -p superduper-pad --test click_audit -- --nocapture
```

### Audition a generated test WAV
```bash
afplay /tmp/pad_click_audit.wav     # macOS — built into the OS
```

### Tail plugin debug logs (during REAPER session)
```bash
tail -F ~/.superduper-dsp/reverb.log
tail -F ~/.superduper-dsp/supermass.log
tail -F ~/.superduper-dsp/pad.log
tail -F ~/.superduper-dsp/ambient.log
tail -F ~/.superduper-dsp/vocal.log
# any plugin with logging: ~/.superduper-dsp/<plugin>.log
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
