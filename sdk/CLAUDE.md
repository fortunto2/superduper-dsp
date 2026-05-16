# sdk — CLAP plumbing + build metadata

Everything every SuperDuper plugin needs that isn't DSP. DSP lives in
`synth-core/`; this crate is *just* boilerplate consolidators.

## Modules

- **`clap_helpers`** — `ParamDef` struct + `write_info` / `write_display` /
  `parse_text` / `init_atomics` static methods. `apply_param_events` reads
  `ParamValueEvent` from the input stream into atomics. `split_io` unifies
  every `ChannelPair` variant into `(read_slice, write_slice)`.
- **`build_meta`** — macros that turn `SDSP_BUILD_*` env vars (from sdk-build)
  into compile-time strings: `plugin_display_name!`, `version_string!`,
  `build_num!`, `build_date!`.
- **`dsp`** — small mono primitives (`OnePole`, `EnvelopeFollower`,
  `DcBlocker`, `soft_clip`, `hard_clip`, `time_to_coeff`). Pre-dates
  synth-core; new shared DSP should go in `synth-core/dsp_blocks` instead
  (this module is kept only because the example-passthrough effect still
  imports from it).

## Adding a new CLAP helper

Three good reasons to land code here:
1. **Boilerplate duplication.** Two effects copy-pasted ~30 lines of the
   same CLAP machinery (params/audio_ports/note_ports). Refactor into a
   helper, both sides shrink.
2. **Easy to get subtly wrong.** Type-system traps like
   `ParamValueEvent::param_id()` returning `Option<ClapId>`. Wrap once,
   document the trap, save future debugging.
3. **CLAP spec semantics.** Anything that pulls from the CLAP spec
   (descriptor flags, port flags, event ordering) belongs centralised.

Bad reasons:
- "Could be useful someday" — wait until two callers exist.
- One plugin's preference — keep it plugin-local.

## Build-meta macros — how they wire up

1. Plugin's `Cargo.toml` puts `superduper-dsp-sdk-build` in `[build-dependencies]`.
2. Plugin's `build.rs` calls `superduper_dsp_sdk_build::emit_build_meta()`.
3. That prints `cargo:rustc-env=SDSP_BUILD_NUM=...` etc.
4. In plugin code, `plugin_display_name!("Base")` expands to
   `concat!("Base [b", env!("SDSP_BUILD_NUM"), "]")` at compile time.

So every plugin needs both `sdk` (regular dep) AND `sdk-build` (build-dep)
for the macros to find their env vars. Forgetting `sdk-build` means
`env!("SDSP_BUILD_NUM")` errors at compile.

## When to update an existing helper

- Bug found by a plugin: fix in helper, every plugin gets the fix.
- New CLAP feature needed by one plugin: keep plugin-local first, promote
  to helper when a second plugin needs the same thing.
- Behaviour change: if the helper is called by multiple plugins, prefer
  adding a new method over changing semantics of the existing one.
