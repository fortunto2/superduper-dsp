---
name: superduper-plugin
description: Scaffold and build a new SuperDuper DSP CLAP plugin (Rust) in the /Users/rustam/Music/1music/superduper-dsp/ workspace. Use when the user asks to create a new effect or instrument plugin in this codebase, port a DSP idea into the SuperDuper format, or set up the boilerplate for a new .clap bundle. Covers the full checklist: workspace registration, build.rs + version macros, params table, audio-ports, GUI with egui_baseview, presets, A/B + LiveScope + state ext, bundle script, clap-wrapper VST3/AU manifest, CI release matrix. Do NOT use for ConjureDSP scripts (different platform — use conjuredsp skill), for using existing plugins in a chain (use sdsp-chain skill), or for REAPER session work (reaper-daw skill).
---

# Scaffolding a new SuperDuper DSP plugin

This codebase ships one CLAP per effect (`SuperDuperReverb.clap`, `SuperDuperSaturator.clap`, …) on a shared foundation: `sdk/`, `sdk-build/`, `synth-core/`. The original "shell with hot-loaded effects" idea is dead — REAPER's per-slot param cache makes dynamic layouts unworkable. Each plugin = its own crate, its own stable CLAP id, a fixed `PARAMS` table.

**Read the project's CLAUDE.md first** — `/Users/rustam/Music/1music/superduper-dsp/CLAUDE.md` is the authoritative reference. This skill is the short checklist.

## Picking your template

| Plugin type | Best template to copy |
|---|---|
| Stereo effect, mostly knobs | `effects/superduper-saturator/` (clean param flow + oversampling) |
| Effect with sidechain | `effects/superduper-reverb/` (Ducker + Sidechain port pattern) |
| Effect with lookahead | `effects/superduper-limiter/` (CLAP `latency` ext) |
| Pass-through analyzer | `effects/superduper-spectrum/` (LUFS meter + LiveScope) |
| Mono → mono utility | `effects/superduper-eq/` (RBJ biquad chain) |
| Linear-phase FIR | `effects/superduper-lineq/` (iFFT FIR + reported latency) |
| Multi-band dynamic / spectral | `effects/superduper-soothe/` (filter bank + baseline detector) |
| Library-of-models / drag-and-drop | `effects/superduper-nam/` (library browser, drop, URL DL) |
| MIDI instrument (poly synth) | `effects/superduper-pad/` (`PadVoice` pool + voice steal) |
| Note-driven synth + curve editor | `effects/superduper-wave/` (mip-pyramid + custom data persist) |
| Physical-model / additive | `effects/superduper-kubyz/` |
| Generator (no MIDI in) | `effects/superduper-ambient/` |

Don't start from `effects/example-passthrough/` — it's the deprecated hot-reload toy.

## Workspace layout for the new crate

```
effects/superduper-<name>/
  Cargo.toml
  build.rs            (one-liner)
  src/
    lib.rs            (CLAP plumbing + DSP entrypoint)
    gui.rs            (egui_baseview window, optional but expected)
    presets.rs        (static factory presets, optional)
    dsp.rs / *.rs     (your actual DSP — keep separate from lib.rs)
  tests/
    dsp_smoke.rs      (drive the block directly, RMS/peak/stability)
    clap_e2e.rs       (clack-host load + process)
    spectrum.rs       (ASCII spectrum via synth_core::analysis)
    quality_audit.rs  (optional: THD / aliasing / IMD numbers)
```

## Full checklist — what to edit

When adding `superduper-<name>` to the codebase, every one of these must be updated. Missing any of them silently breaks distribution.

### 1. Crate scaffolding

- [ ] `effects/superduper-<name>/Cargo.toml` — copy from saturator/reverb, change `name`, `[lib].name`, `description`. Keep all GUI deps pinned to the exact versions used by reverb (egui 0.33, egui-baseview git, baseview pinned to `237d323c...`).
- [ ] `effects/superduper-<name>/build.rs` — one line:
  ```rust
  fn main() { superduper_dsp_sdk_build::emit_build_meta(); }
  ```
- [ ] `effects/superduper-<name>/src/lib.rs` — see "lib.rs skeleton" below.
- [ ] `effects/superduper-<name>/src/gui.rs` — egui window. Use `superduper_synth_core::gui as core_gui` helpers: `top_bar`, `section`, `param_row`, `ab_init_bar`, `LiveScope`.
- [ ] `effects/superduper-<name>/src/presets.rs` — `&[Preset]` static slice using `define_preset!` from SDK or `Preset::from_overrides` (typo-proof sparse initialiser).
- [ ] Tests: minimum `dsp_smoke.rs` + `clap_e2e.rs` + `spectrum.rs`.

### 2. Workspace registration

- [ ] `Cargo.toml` (root) — add `"effects/superduper-<name>"` to `[workspace] members`.
- [ ] `scripts/build_<name>_bundle.sh` — copy `scripts/build_saturator_bundle.sh` and replace every `superduper-saturator` / `SuperDuperSaturator` / `saturator` / `co.superduperai.saturator` / `libsuperduper_saturator.dylib`. Keep `chmod +x` on the new file.
- [ ] `cmake/plugin_list.cmake` — add a row to `SDSP_WRAPPER_PLUGINS`:
  ```
  "superduper-<name>|SuperDuper<Name>|co.superduperai.<name>|co.superduperai.wrappers.<name>|aufx|sd<XX>"
  ```
  Effects → `aufx`, instruments → `aumu`. Sub-code is 4 chars, must be unique across the table.
- [ ] `.github/workflows/release.yml` — add the new bundle script to both the macOS and Windows build matrices (mirror an existing entry — search for `build_saturator_bundle`).
- [ ] `tools/sdsp-chain/Cargo.toml` + `tools/sdsp-chain/src/main.rs` — add as static dep + `impl_stage!` entry (only if the plugin makes sense in a mastering chain).
- [ ] Project `CLAUDE.md` — bump the plugin count + add a one-line entry under "Current state".
- [ ] `README.md` — add the plugin row (mirrors README format).

### 3. CI sanity

After the first push to a branch, the GH Actions matrix should green on both macos-14 and windows-latest. Tag the release once everything is in place (`v0.X.0`).

## `src/lib.rs` skeleton — what every plugin has

Crib from `effects/superduper-saturator/src/lib.rs`. Key contracts:

```rust
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_num, build_date, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{SmoothedParam, DcBlocker /* … */};

// Fixed param table — NEVER add/remove/reorder after shipping. Display
// names + ranges live here; no hard-coded numbers in process().
const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Drive",  min: 0.0,  max: 24.0, default: 0.0,  unit: "dB" },
    ParamDef { id: 1, name: b"Mix",    min: 0.0,  max: 1.0,  default: 1.0,  unit: "" },
    // …
];
const P_DRIVE: usize = 0;
const P_MIX: usize  = 1;

// Shared atomic state — accessed from GUI thread + audio thread.
pub struct PluginShared {
    pub inner: Arc<SharedParamsInner>,  // Arc so GUI thread can clone
}
pub struct SharedParamsInner {
    pub params:        [AtomicF32; PARAMS.len()],
    pub dirty_params:  [AtomicBool; PARAMS.len()],
    pub gesture_begin: [AtomicBool; PARAMS.len()],
    pub gesture_end:   [AtomicBool; PARAMS.len()],
    pub bypass:        AtomicBool,
    pub scope:         core_gui::LiveScope,  // for top spectrum strip
}

// CLAP audio ports — stereo in/out is the floor.
impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 { if is_input { 2 } else { 1 } }
    fn get(&mut self, idx: u32, is_input: bool, w: &mut AudioPortInfoWriter) {
        match (idx, is_input) {
            (0, _)     => /* IS_MAIN, in_place_pair: Some(ClapId::new(0)), STEREO */,
            (1, true)  => /* Sidechain, no IS_MAIN, in_place_pair: None */,
            _ => {}
        }
    }
}

// Param flush + value <-> text plumbing.
impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 { PARAMS.len() as u32 }
    fn get_info(&mut self, i, info)  { ParamDef::write_info(PARAMS, i, info); }
    fn value_to_text(&mut self, id, v, w) -> _ { ParamDef::write_display(PARAMS, id, v, w) }
    fn text_to_value(&mut self, id, t) -> _    { ParamDef::parse_text(PARAMS, id, t) }
    fn flush(&mut self, ev, _) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.inner.params, ev);
    }
}

// process() — pull from ParamValueEvent into atomics, run DSP, then
// emit dirty-param events + gesture events back to the host so REAPER
// records knob moves into automation lanes.
fn process(/* … */) {
    apply_param_events(&shared.params, events.input);
    // … run DSP, push to scope ring buffer …
    emit_dirty_param_events(&shared.params, &shared.dirty_params, events.output);
    emit_gesture_events(&shared.gesture_begin, &shared.gesture_end, events.output);
}

// Descriptor — display name and version get the [bNNNNN] suffix.
impl DefaultPluginFactory for ... {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.<name>",                  // stable CLAP id — never change
            plugin_display_name!("SuperDuper <Name>"), // → "SuperDuper <Name> [bNNNNN]"
        ).with_version(version_string!("0.1"))          // → "0.1.NNNNN (YYYY-MM-DD)"
        // … vendor, features
    }
}

clack_export_entry!(SinglePluginEntry<...>);
```

## Reusable building blocks — use these instead of rolling your own

### `superduper_dsp_sdk::clap_helpers`
- `ParamDef` — declare once, plug into the CLAP param-info trait.
- `apply_param_events` — copies `ParamValueEvent` into atomics.
  **`pv.param_id()` returns `Option<ClapId>`** — destructure or you silently drop events.
- `emit_dirty_param_events` — flush GUI knob moves back to host.
- `emit_gesture_events` — flush `drag_started` / `drag_stopped` → host touch automation.
- `save_simple_state` / `load_simple_state` — JSON-versioned state for CLAP `state` ext.
- `split_io` / `output_slice` — normalise InputOutput / InPlace / OutputOnly buffers. **Synth plugins must use `output_slice`** — `split_io` returns `None` for OutputOnly and produces silence.

### `superduper_dsp_sdk` macros (need `sdk-build` in build-deps)
- `plugin_display_name!("Base")` → `"Base [bNNNNN]"`
- `version_string!("0.X")` → `"0.X.NNNNN (YYYY-MM-DD)"`
- `simple_state_impl!(Plugin, version, …)` — boilerplate for CLAP state ext.
- `define_preset!` — typo-proof preset initialiser.

### `superduper_synth_core::dsp_blocks`
- `Ducker` — sidechain peak gain reducer (asymmetric att/rel).
- `Tilt` — single-shelf brightness ±6 dB.
- `DcBlocker` — first-order HPF ~38 Hz. **Put before any feedback loop**.
- `SmoothedParam` — one-pole interp. Snap to initial in `activate()` so the first block isn't a fade-in.
- `Biquad` — RBJ peaking / shelf / HPF / LPF, Direct Form II Transposed.
- `EnvelopeDetector` — asymmetric one-pole peak follower.
- `compressor_gain_db` + `CompressorCurve` (Clean/Pump/Smooth) — soft-knee + GMR equations.
- `Oversampler2x` + `oversample_apply` — 11-tap halfband for any per-sample non-linearity.
- `DelayLine` — variable length + 3rd-order Lagrange interp (don't use linear).
- `SlewLimiter2Pole` — C¹-continuous slew for delay-time / pitch (single one-pole clicks).
- `OnePoleLp` — feedback-tone control in delays.
- `PadVoice` + `PadParams` — TPT/ZDF SVF + tanh, supports voice steal without click.
- `AdsrEnvelope` + `AdsrParams` — linear-attack / exp-decay, `gate_on()` resumes from current level.
- `midi_note_to_hz`, `tanh_drive`, `tape_clip`, `tube_clip`, `sync_division_hz`.

### `superduper_synth_core::analysis`
- `magnitude_spectrum_db`, `spectrum_with_freq` — Hann + real FFT.
- `ascii_spectrum` — bar chart for `cargo test ... -- --nocapture` assertions.
- `frequency_response_sine_sweep` — measure response of a closure.
- `measure_thd_db`, `measure_aliasing_db`, `measure_imd_smpte_db` — quantitative quality.
- `make_bin_aligned_sine` — sine that lands exactly on an FFT bin (use for every spectrum assertion).

### `superduper_synth_core::loudness`
- `KWeighting` + `LoudnessMeter` + `TruePeakDetector` — BS.1770-4 LUFS-M/S/I + dBTP. Calibrated against 1 kHz sine.

### `superduper_synth_core::linphase`
- `design_linear_phase_fir(target_mag, fir_len)` → symmetric FIR via iFFT + Hann.
- `DirectFirConvolver` — RT-safe circular-history convolver.

### `superduper_synth_core::user_preset`
- `PresetRepo<E: PresetExtra>` — file-backed presets at `~/.superduper-dsp/<plugin>/presets/*.json` + `last.json` auto-save.
- `PresetName` value object, `PresetError` enum, validation.

### `superduper_synth_core::supermass`
- `build_wet()` — fundsp `Net` for the cascade reverb. Build in `activate()`, never rebuild.

### `superduper_synth_core::nam`
- `WaveNet` / `Lstm` / `Linear` — pure-Rust port of NAM C++ Core inference.
  Sample-by-sample, RT-safe. Weight ordering bit-compatible with NAM 0.5.x
  `.nam` JSON files. WaveNet supports gating_mode (None/Gated/Blended) +
  secondary_activation + head1x1; FiLM is intentionally not supported
  (no community models use it).
- `NamModel` enum — uniform handle over all three architectures so a
  host plugin can hold one type regardless of what the user loaded.
- `load_from_json(text)` → `NamFile`; `NamModel::from_nam_file(&file)`
  validates and builds. Returns typed `NamError::UnsupportedArch`,
  `UnsupportedFeature`, `WeightCountMismatch` so the plugin can show
  a clear message and grey-out the file in its library.

### `superduper_synth_core::gui` (`feature = "gui"`)
- `top_bar(ui, name, build, ver, bypass, combo_id, presets, &mut selected)`
- `section(ui, title, |ui| { … })`
- `param_row(ui, &shared.params[P_X], &PARAMS[P_X])`
- `dirty_param_row` / `learn_param_row_g` — for plugins with MIDI learn or gestures
- `dirty_toggle_row_g(ui, atom, def, dirty, gesture, idx)` — LED-style boolean toggle (use for `On`/`Off` style params instead of a slider)
- `dirty_choice_row_g(ui, atom, def, &options, dirty, gesture, idx)` — radio row for enum-style params (Type/Mode/Curve etc.)
- `help_block(ui, id, &[(heading, body)])` — collapsible in-plugin docs
- `help_block_with_links(ui, id, &[(heading, body, &[(label, url)])])` — same + clickable URL chips
- `link_button(ui, label, url)` — single clickable link (shells out to `open` / `start` / `xdg-open`)
- `ab_init_bar(ui, &shared, &AbSnapshot)` — A / B / copy / init buttons
- `LiveScope` — lock-free ring buffer for the spectrum strip
- `draw_spectrum_strip(ui, scope, rect, sr)` — log-Hz spectrum with grid
- `draw_spectrum_marker_colored(ui, rect, label, freq_hz, gr_db, color, show_db)` — vertical dashed pointer overlay
- `draw_spectrum_band_overlay(ui, rect, f_lo, f_hi, fill_color)` — translucent zone overlay
- `install_default_style`, `ResizeBridge`, `new_resize_bridge`, `read_bridge`

## Hard-won lessons (the ones that break new plugins most often)

1. **`audio-ports` extension is mandatory.** REAPER calls `process()` but routes no audio without it. `IS_MAIN` + `in_place_pair: Some(ClapId::new(0))`.
2. **`ParamValueEvent::param_id()` returns `Option`**, not `ClapId`. Direct `==` compiles and is always false.
3. **CLAP crate versions are 0.1.x.** Don't trust upstream 0.4 examples.
4. **No `#![no_std]`** — `f32::exp/tanh/powf` need std.
5. **`~/.local/bin/python3.*` is Pyodide WASM.** Use `/opt/homebrew/bin/python3.13` for native tooling.
6. **`notify::recommended_watcher()` blocks on macOS FSEvents init.** Never in `new_shared()` — REAPER's scan serialises and you get 5 s per plugin.
7. **stderr from Dock-launched DAWs is dropped.** Log to `~/.superduper-dsp/<plugin>.log` via per-plugin `OnceLock<Mutex<File>>` + `slog!` macro. Gate behind a feature for shipping (file I/O ≠ RT-safe).
8. **`CARGO_TARGET_DIR=/Users/rustam/.cargo-target`** is set globally — bundle scripts already handle it.
9. **fundsp `Net` is RT-unsafe to rebuild.** Build in `activate()`, call `set_sample_rate`, never rebuild.
10. **REAPER caches param layouts per (plugin_id, FX-slot).** Changing `PARAMS` after shipping breaks user projects. If you must, bump the CLAP id (and forfeit user automation).
11. **No heap in `process()`.** Pre-allocate scratch buffers as `Box<[f32]>` in `activate(max_frames_count)`. Mirror the reverb sidechain-snapshot shape.
12. **DC blocker before any feedback loop.** Otherwise DC accumulates and drowns the tail.
13. **Smooth user-facing params.** Atomic-load per-sample is fine, but the target steps; `SmoothedParam` kills zipper noise on knob drags.
14. **MIDI synths: declare BOTH note dialects.** `NoteDialects::CLAP | NoteDialects::MIDI`, `preferred_dialect: Some(NoteDialect::Clap)`. Hosts pick one; missing dialect = silent drop.
15. **Synths: `ChannelPair::OutputOnly` is real.** Use `clap_helpers::output_slice`, not `split_io`.
16. **TPT/ZDF SVF over Chamberlin.** Chamberlin blows up above `sr/6`.
17. **Voice steal — preserve oscillator + filter state.** Only reset `key/note_id/velocity/age` + `env.gate_on()`. Zeroing `lp_z1/lp_z2` clicks.
18. **Drag-knob audio vanish = slew too fast on log params.** 30-50 ms time constant, or convert to octaves/sec.
19. **Sample-discontinuity audit pattern.** `tests/click_audit.rs` drives the plugin via clack-host, writes `/tmp/<name>_click_audit.wav`, asserts `max |x[n+1] - x[n]| < 0.4`. WAV doubles as listening test.
20. **CLAP `latency` ext for any pre-delay.** Without it, DAW doesn't PDC and the parallel bus phases out.
21. **Automation write — emit `ParamValueEvent` on dirty bits.** Just storing into `AtomicF32` isn't enough; REAPER won't record without events from `process()`.
22. **MIDI CC handlers must NOT raise dirty bit.** Otherwise CC → ParamValueEvent → CC envelope → CC handler = feedback loop.
23. **CLAP `state` ext for any custom data.** Drawn curves / harmonic bars / formants disappear without it. Use `simple_state_impl!`.
24. **`apply_preset` must mark every param dirty.** Otherwise preset switches don't get into the automation lane.
25. **Host BPM lives in the `Transport` event.** Cache in shared `AtomicF32`; use `synth_core::dsp_blocks::sync_division_hz` for tempo-synced rates.
26. **One scope ring buffer per plugin = atomic per slot.** `core_gui::LiveScope` is lock-free.
27. **Gestures are independent of value changes.** A drag with no value change still wants `Begin/End` for touch-automation latch. Read `slider_resp.drag_started/drag_stopped` from egui.

## DSP code style — never violate inside `process()`

- No heap (`Vec`, `Box::new`, `String`, `HashMap`).
- No `Mutex` / `RwLock` — atomics only.
- No syscalls (`println!`, file I/O, networking).
- No `panic!`, `unwrap`, `expect`, unchecked `arr[i]`.
- `core::f32::*` math is OK (std linked).
- `static mut` for module-level state is OK with `unsafe` + audio-thread-only comment.

## Build + iterate workflow

```bash
cd /Users/rustam/Music/1music/superduper-dsp
./scripts/build_<name>_bundle.sh        # installs to ~/Library/Audio/Plug-Ins/CLAP/
cargo test --release -p superduper-<name>
cargo test --release -p superduper-<name> --test spectrum -- --nocapture

# Audition through CLI (effects only):
cargo run --release -p sdsp-runner -- \
  ~/Library/Audio/Plug-Ins/CLAP/SuperDuper<Name>.clap input.wav

# Multi-plugin chain (if added to sdsp-chain):
cargo run --release -p sdsp-chain -- chain.toml in.wav out.wav

# REAPER cache problems: Preferences → Plug-ins → CLAP → Clear cache + re-scan.
# Build numbers [bNNNNN] in display name tell which build is loaded.
./scripts/restart_reaper.sh
```

## When in doubt

- **Read the project `CLAUDE.md`** at `/Users/rustam/Music/1music/superduper-dsp/CLAUDE.md` — single source of truth.
- **Copy the closest existing plugin** rather than starting from scratch.
- **Ask the user** before locking in a `PARAMS` table — once shipped, it's frozen.
