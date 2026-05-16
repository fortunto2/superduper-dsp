# M5: Custom CLAP GUI (egui-baseview)

## Why

REAPER (and apparently most CLAP hosts) caches parameter info at plugin
activate-time and never re-reads it for an instance that stays alive. So
swapping the effect dylib at runtime — which can change param names,
ranges, units, count — updates audio + values but leaves UI labels stale.
`host.request_restart` + `params.rescan(INFO)` don't fix it; verified
across v1–v4 of the plugin id.

ConjureDSP and most "AI-authored DSP" plugins side-step this by drawing
their own UI inside the plugin window via the CLAP `gui` extension. The
host just hands the plugin a window and the plugin renders whatever it
likes — REAPER never queries param info to populate UI, so the layout
mismatch can't happen.

## Scope

Bring SuperDuper DSP up to the same UI architecture:

1. **Plugin reports `clap.gui` extension** with macOS Cocoa as preferred API.
2. **Plugin spawns an egui surface** in the host-provided NSView via
   `egui-baseview` 0.1+.
3. **egui draws the live state**:
   - "SuperDuper DSP — Effect: <current name>" header.
   - Dropdown / list to pick from the catalog (uses the same
     `effect_load_pending` channel currently driving the CLAP param).
   - One slider per `slot.meta().params[i]` with name/min/max/unit from
     the effect's metadata. Sliders read from / write to
     `shared.effect_params[i]` atomics — same source the audio thread uses.
   - Reload button hitting `shared.reload_requested`.
   - Bypass toggle hitting `shared.bypass`.
4. **MCP integration stays intact**: tools/get_params returns the same
   data the GUI reads, set_param writes the same atomics. Source of truth
   is shared state, GUI is one renderer.

## Crates

- `baseview = "0.1"` — windowing on top of the host NSView.
- `egui-baseview = "0.1"` — egui ↔ baseview bridge.
- `egui = "0.27"` — UI toolkit (whatever egui-baseview 0.1 pins).
- `clack-extensions` already has the `gui` feature flag we'd enable.

## CLAP gui lifecycle (cheatsheet)

```text
host                          plugin
----                          ------
is_api_supported("cocoa")  →  true on macOS
get_preferred_api          →  ("cocoa", floating=false)
create("cocoa", false)     →  build state, return true
get_size                   →  return (width, height)
set_parent(NSView*)        →  spawn egui-baseview window inside that view
show                       →  render begin
                              [user interacts with our widgets]
                              [we read/write shared atomics]
hide                       →  pause rendering
destroy                    →  drop window, drop egui ctx
```

The dance happens on the main thread, so we can freely touch
`PluginShared` from the GUI thread provided we go through the existing
atomic / `ArcSwap` machinery — no extra locks needed.

## Tricky bits we'll hit

- **macOS event loop integration**: baseview owns the run loop for its
  window. REAPER also runs the main event loop. They need to cooperate
  — baseview supports both "parented" (REAPER drives the loop) and
  "embedded" (we drive a sub-loop). Use parented.
- **DPI / scaling**: REAPER passes `set_scale(scale)` — egui needs
  `EguiContext::set_pixels_per_point`.
- **Param events back to host**: when the user drags an egui slider, we
  need to emit `ParamValueEvent`s on the output queue so REAPER records
  automation. Plumb through the `output_parameter_changes` we already
  receive in `PluginAudioProcessorParams::flush`.
- **Resize**: opt out of resizing initially (`can_resize: false`,
  fixed `get_size`). Resize support is M6+.

## Milestones inside M5

- **M5.1** — Add gui feature flag + clack-extensions deps. Implement
  `PluginGuiImpl` with all methods returning conservative defaults
  (cocoa supported, fixed 480×320, no resize). REAPER shows an empty
  embedded window. Commit.
- **M5.2** — egui-baseview window inside `create()`. Renders just
  "SuperDuper DSP" text on a solid background. Confirms event loop
  integration. Commit.
- **M5.3** — Wire up read-only widgets: Gain slider, Bypass toggle,
  Effect dropdown, Reload button. State flows from shared atomics to
  egui only. Param edits from host (automation, CLAP events) reflect
  in egui live.
- **M5.4** — Two-way binding: dragging a slider in egui pushes the
  new value into shared atomics AND emits `ParamValueEvent` on the
  output queue so REAPER records automation correctly.
- **M5.5** — Per-effect sliders (dynamic count from `slot.meta()`).
  This is the headline feature — labels now follow the effect, not
  the host's cached layout, because we draw them ourselves.
- **M5.6** — Remove the workaround `Reload` and `Effect ▼` CLAP params
  (they were UI proxies for REAPER's generic UI; with custom UI we own
  the affordance directly). Bump plugin id to v5 for the cleanup.

## Out of scope for M5

- Window resizing / multi-DPI handling beyond a fixed size.
- Save/restore GUI-only state (position, theme).
- Animations, custom theming.
- A code editor pane (we said "no editor inside the plugin" by design
  in SPEC.md — the user lives in the terminal).

## Testing

Cargo test won't catch GUI bugs, but we can add:

- A `cargo test --release -p superduper-dsp-plugin --features gui-headless`
  variant that constructs the egui state without a real window and asserts
  that widget callbacks update the shared atomics correctly.
- Manual smoke test in REAPER: load each of the 6 catalog effects, verify
  slider labels match effect's `params!` declarations, verify slider
  motions show up in `mcp__superduper-dsp__get_params` output.

## Why not just live with v4?

We can. v4 works for the AI-first workflow (Claude writes effects via
MCP, hot-loads, plays through them). What v4 doesn't give:

- Honest UI labels when the user manually switches via the Effect
  dropdown — they see brick_limiter's THRESHOLD/RELEASE labels even
  after switching to plate_reverb.
- Per-effect knob layouts (some effects have 2 params, some have 5;
  REAPER renders all 32 slot widgets at fixed positions).

If those don't matter (Claude reads params via MCP `get_params` and
drives them via `set_param` — no human is squinting at the FX panel)
v4 is the right stopping point and we move on to other features
(sample_rate in effect ABI, Stage C freeze, FTZ ARM64).
