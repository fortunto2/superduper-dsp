# SuperDuper DSP

Headless CLAP plugin + companion daemon. Claude writes DSP code, the plugin hot-loads it as native Rust.

**Status:** M1 (Hello CLAP + Daemon handshake) — work in progress.

## What this is

```
┌──────────────┐     MCP (HTTP/SSE)      ┌────────────────┐
│ Claude Code  │ ◄─────── :7891 ─────────►│ superduperd    │
└──────────────┘                         │ (daemon)       │
                                         └───────┬────────┘
                                                 │ Unix socket
                                                 │
                                    ┌────────────┴───────────┐
                                    │                        │
                          ┌─────────▼───────┐    ┌───────────▼─────┐
                          │ SuperDuperDSP   │    │ SuperDuperDSP   │
                          │ .clap (track 1) │    │ .clap (track 2) │
                          └─────────────────┘    └─────────────────┘
```

You add SuperDuper DSP to a track in REAPER (or any CLAP host). In your terminal, you
talk to Claude Code. Claude generates Rust DSP code and pushes it through the MCP server.
The daemon compiles it (`cargo build --release --crate-type cdylib`), the plugin
`dlopen`s the fresh `.dylib`, atomically swaps the `process` function pointer, and the
new effect is playing — typically 1–3 seconds end-to-end.

No code editor inside the plugin. No GUI prompt fields. You live in the terminal.

## Why this exists

`RS5k manager` + `MPL framework` + `SWS` + `ReaImGui` + `js_ReaScriptAPI` — five layers,
each a failure point. SuperDuper DSP is one plugin and one daemon, both Rust, both yours.

Also: it's the simplest possible interface for AI-generated audio. The plugin doesn't
know what an effect is — it just runs the function pointer. The intelligence is in
Claude Code, where it belongs.

## Setup

```bash
git clone https://github.com/superduperai/superduper-dsp
cd superduper-dsp

# Build everything
cargo build --release

# Bundle and install the .clap for macOS
./scripts/build_bundle.sh
./scripts/install_local.sh
```

Then in REAPER: FX browser → "SuperDuper DSP" (instrument/utility category).

Configure Claude Code to use the MCP server:

```json
{
  "mcpServers": {
    "superduper-dsp": {
      "url": "http://127.0.0.1:7891/sse"
    }
  }
}
```

## Workflow

```
$ claude
> add a tape saturation to the Lead track
[Claude generates code, calls load_effect via MCP, daemon compiles, plugin loads]
> warmer, less drive
[Claude calls set_param]
> save as 'warm_lead'
[Claude calls save_session]
```

## Roadmap

| Milestone | Status | What |
|---|---|---|
| M1 | 🚧 | Hello CLAP + Daemon handshake |
| M2 | ⏳ | Full MCP server with all tools |
| M3 | ⏳ | Hot-reload Rust code |
| M4 | ⏳ | Params system with CLAP rescan |
| M5 | ⏳ | Minimal GUI (status + bypass + name) |
| M6 | ⏳ | Sessions save/load |
| M7 | ⏳ | Polish, examples, v0.1 release |

## Project layout

```
superduper-dsp/
├─ plugin/                 CLAP plugin (loads in DAW)
├─ daemon/                 superduperd (MCP server + build orchestrator)
├─ protocol/               Shared IPC and MCP types
├─ sdk/                    superduper-dsp-sdk (used by user effect crates)
├─ effects/                Example effect crates
│  └─ example-passthrough/ Reference: gain + soft clip
├─ scripts/                Build & install scripts
├─ SPEC.md                 Full design doc
├─ CLAUDE.md               Instructions for Claude Code
└─ README.md               This file
```

## Why Rust?

- Same stack as the rest of SuperDuperAI infrastructure
- No GC pauses, no allocator surprises in audio thread
- Cargo handles hot-reload compilation trivially
- Claude Code writes Rust DSP code excellently

## Inspired by

- **ConjureDSP** — for the live-coded DSP plugin concept (Python + Rust dual mode)
- **Live coding scenes** — Extempore, SuperCollider, Glicol (hot-reload code at audio time)
- **ReaScript** — for showing that programmable DAWs are powerful

But none of those let you say "soft tape saturation" in plain English and have it
happen. That's the gap SuperDuper DSP fills.

## License

MIT. See LICENSE.

## Part of the SuperDuperAI family

- **SuperDuperAI** — AI-first creative tools (video editor, mobile inference)
- **Akbuzat** — decentralized mesh messenger (Bashkir mythology)
- **SuperDuper DSP** — this project, AI-driven DSP runtime
