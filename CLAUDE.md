# Claude Code instructions for SuperDuper DSP

Plugin platform: CLAP plugin (Rust, `clack-plugin 0.1` + `clack-extensions 0.1`) that
loads user-written DSP effects as native `.dylib` files with hot-reload via
`libloading`. AI authors `process.rs` files; the watcher picks up rebuilt dylibs
and atomically swaps the function pointer.

## Current state

- **M0**: Hello CLAP with one `Gain` parameter (−24..+24 dB). DONE.
- **M1**: native dylib hot-reload via `HotReloadSlot` + `notify` file watcher,
  `catch_unwind` safety net, ABI handshake (`sdsp_protocol_version`). DONE.
- **M2** (next): proc-macro `params!` / `effect!` to cut effect boilerplate to ~10 LOC.
- **M3** (later): in-plugin MCP server (axum+SSE) — Claude Code drives `load_effect`.

## Hard-won architectural lessons

Read these before editing anything CLAP-related:

1. **`audio-ports` extension is mandatory** even for a stereo-in/stereo-out
   effect. Without `builder.register::<PluginAudioPorts>()` in `declare_extensions`,
   REAPER calls `process()` but routes no audio through it — the plugin appears
   to load, parameters work, but the signal bypasses you. Symptom: `Gain` slider
   moves in the UI but volume doesn't change. We hit this hard. The audio-ports
   impl must declare a stereo port with `AudioPortFlags::IS_MAIN` and ideally
   `in_place_pair: Some(ClapId::new(0))` to let the host pick in-place processing.

2. **`ParamValueEvent::param_id()` returns `Option<ClapId>`, not `ClapId`.** Do
   not write `if pv.param_id() == target_id { ... }` — that silently always
   evaluates to false (it compiles via a blanket PartialEq). Always destructure:
   `let Some(id) = pv.param_id() else { continue }; if id == target_id { ... }`.

3. **CLAP versions in the registry are 0.1.x, not 0.4.x.** The original Cargo.toml
   from the project skeleton pinned `clack-plugin = "0.4"` which doesn't exist
   on crates.io. The actual current release is 0.1.0. If you regenerate from
   scaffolding, double-check what `cargo search clack-plugin` returns.

4. **`#![no_std]` in SDK breaks `f32::exp()` and `.tanh()`.** They come from
   `std::f32`. Either drop `no_std` (current choice) or pull in `libm` and call
   `libm::expf` / `libm::tanhf`. Real effects almost always link std anyway,
   so `no_std` buys nothing right now.

5. **`~/.local/bin/python3.*` on this Mac is Pyodide (WASM), not native CPython.**
   For Rust builds you don't care about this, but if you ever wire in Python
   tooling (e.g. for codegen scripts), use `/opt/homebrew/bin/python3.*` instead.

6. **`notify::recommended_watcher()` must NOT be called during plugin scan.**
   It blocks briefly while FSEvents initialises on macOS, and REAPER's CLAP
   scan path serialises everything — calling it from `new_shared()` made the
   whole REAPER main thread time out for ~5 seconds per call. Defer to
   `activate()` (audio thread setup) via a `parking_lot::Mutex<Option<...>>`
   + `ensure_watcher()` idempotent init. Already done; don't undo it.

7. **stderr from a Dock-launched plugin host is dropped.** `tracing_subscriber`
   writing to stderr won't show up in `log stream --process REAPER`. For debug
   logs, write to `~/.superduper-dsp/plugin.log` directly (we have a `dlog!`
   macro for this). Not RT-safe, but acceptable during development. Tail with
   `tail -F ~/.superduper-dsp/plugin.log`.

8. **CARGO_TARGET_DIR is set globally on this machine to `/Users/rustam/.cargo-target`.**
   `scripts/build_bundle.sh` respects `${CARGO_TARGET_DIR:-./target}` for that
   reason. Don't hardcode `./target`.

## DSP code contract (for effects in `effects/*/`)

When generating a `process.rs` for an effect, follow this exact contract.

```rust
//! Effect: short description.
#![allow(clippy::missing_safety_doc)]

/// Audio block process — called from RT thread. NO ALLOC, NO PANIC, NO IO.
#[no_mangle]
pub unsafe extern "C" fn process(
    input: *const f32,
    output: *mut f32,
    channel_count: u32,
    frame_count: u32,
    _params: *const f32,   // unused until M2 ships params!
) {
    let total = (channel_count as usize) * (frame_count as usize);
    for i in 0..total {
        *output.add(i) = *input.add(i); // your DSP here
    }
}

/// ABI handshake. Plugin refuses to load if this doesn't match.
#[no_mangle]
pub extern "C" fn sdsp_protocol_version() -> u32 { 1 }

/// Param metadata as a null-terminated JSON byte slice. Empty until M2.
#[no_mangle]
pub extern "C" fn sdsp_param_descriptor_json() -> *const u8 {
    static JSON: &[u8] = b"[]\0";
    JSON.as_ptr()
}
```

### Hard rules — never violate

- No `std::alloc` — no `Vec`, `String`, `Box`, `HashMap` allocations in `process()`.
  Stack arrays or module-level `static mut` only.
- No `Mutex` / `RwLock` — atomics only.
- No syscalls — no file I/O, no `println!`, no networking.
- No `panic!`, no `unwrap()`, no `expect()`, no array `[i]` indexing that can
  fail. Use `.get()` with `Option` or `unsafe { *ptr.add(i) }` after manually
  bounds-checking against `frame_count`.
- Panics are caught by `catch_unwind` on the plugin side and poison the slot,
  but that's a safety net, not a coding style.

### What you CAN do

- Everything in `superduper_dsp_sdk::dsp::` (`OnePole`, `EnvelopeFollower`,
  `DcBlocker`, `soft_clip`, `hard_clip`, `time_to_coeff`).
- `core::f32::*` math (`.sin()`, `.tanh()`, `.powf()`, etc.) — we link std.
- Small stack arrays: `let mut state = [0.0_f32; 64];`.
- `static mut` for per-effect persistent state. Wrap accesses in `unsafe`.

## Workflow

### Build the plugin
```bash
cargo build --release
./scripts/build_bundle.sh        # produces dist/SuperDuperDSP.clap
./scripts/install_local.sh       # symlinks/copies to ~/Library/Audio/Plug-Ins/CLAP/
```

### Hot-reload an effect (M1 manual workflow)
```bash
cargo build --release -p example-passthrough
./scripts/load_effect.sh         # copies into latest ~/.superduper-dsp/instances/*/effect.dylib
```
Watcher picks it up in ~250ms; REAPER keeps playing through the new effect.

### Run tests
```bash
cargo test -p superduper-dsp-plugin
# 9 tests: 4 lib unit + 2 hotreload unit + 3 hotreload integration
```

### Watch debug logs
```bash
tail -F ~/.superduper-dsp/plugin.log
```

## Distribution model — A+C hybrid (decided 2026-05)

Future stages will let users ship AI-generated effects to others without Claude:

**Stage A (M2–M3): shell + effects folder.** One `SuperDuper DSP.clap` scans
`~/Library/Audio/Plug-Ins/SuperDuper Effects/*.dylib` and exposes the list as
a CLAP enum-stepped parameter. Switching the effect triggers `slot.swap()`.
Sharing = passing a `.dylib` file. Watcher migrates from per-instance dir to
this shared folder.

**Stage C (M5+): freeze to standalone `.clap`.** MCP tool `freeze(name, vendor)`
generates a Cargo template instance with `include_bytes!`-embedded dylib + a
unique plugin ID, builds it, produces `<name>.clap` — a self-contained plugin
that shows up natively in FX browsers. Sharing = passing one `.clap`.

**NOT doing:** B (one .clap containing N plugins via factory) — runtime
complexity without distribution win over C.

## Future MCP tools (M3)

Once the in-plugin MCP server lands, these will be exposed under `superduper-dsp`:
- `list_instances()` — see all live plugin instances
- `load_effect(target, code)` — compile and hot-load
- `get_params(target)` / `set_param(target, name, value)`
- `bypass(target, enabled)`
- `save_session(name)` / `load_session(name)`
- `get_code(target)` / `get_status(target)`

For now, only `track_fx_*` via the REAPER MCP (`total-reaper-mcp`) can drive the
plugin externally — and even that has quirks (CLAP `set_param` via REAPER's
ScriptAPI may not propagate plain values correctly to the plugin; UI-driven
crank works fine via `ParamValueEvent`s through `process()`).

## When in doubt

Ask the user. Better one clarification question than three failed compiles.
