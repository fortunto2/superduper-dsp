# SuperDuper DSP: agent skills

Nine skills that teach an AI agent to make music with [SuperDuper DSP](https://github.com/fortunto2/superduper-dsp),
an open-source suite of CLAP audio plugins written in Rust. They cover mixing and mastering by measurement,
songs built in REAPER on the suite's instruments, headless plugin chains, audio to MIDI, mashups, reference
teardowns and vocal chains, and the checklist for writing a new plugin.

Project page: <https://rustman.org/projects/project-superduper-dsp/> · How it is used, in plain words: <https://rustman.org/wiki/creative-music-with-claude/> · Repository: <https://github.com/fortunto2/superduper-dsp>

**What you need before it does anything useful:** the SuperDuper DSP plugins and command-line tools (see Prerequisites), plus Python for the measurement
scripts. Two skills also need REAPER and a REAPER MCP server. Each skill checks and tells you what is missing.

## Skills

| Skill | What it does |
|---|---|
| `sdsp-mix` | Mix and master by measurement: band balance against a reference, sidechain ducking, mono low end, loudness targets. Ships `mixcheck.py`, `mixverify.py`, `mixopt.py`, `mixmap.py`. |
| `sdsp-chain` | Render plugin chains headlessly from a TOML file (no DAW), with per-stage LUFS and true peak. |
| `superduper-song` | Build songs in REAPER on the SuperDuper instruments through a REAPER MCP server: parameter maps, presets, MIDI CC. |
| `audio-to-midi` | Turn a recording into MIDI with a local model, and check the result before trusting it. |
| `track-teardown` | Reverse-engineer a reference track with spectrograms, stems and numbers, and rebuild its feel. |
| `vocal-mix` | A slot-by-slot vocal chain in REAPER, with the numbers that shipped on a real track. |
| `mic-compare` | Compare two microphones or takes by frequency response, noise floor, SNR and plosives. |
| `sdsp-mash` | Mashups and multi-verse cyphers with the `sdsp-mash` CLI. |
| `superduper-plugin` | Scaffold and build a new CLAP plugin inside a checkout of the repository. |

## Install

In Claude Code:

```bash
/plugin marketplace add fortunto2/superduper-dsp
/plugin install superduper-dsp@superduper-dsp
```

The plugin is only instructions and a few Python scripts. The audio software it drives is installed separately.

## Prerequisites

Each skill starts with a "Needs" note and checks for what it needs before running, so a missing tool is reported rather than hit halfway.

- **SuperDuper DSP plugins and command-line tools** (`sdsp-chain`, `sdsp-bus`): on macOS (Apple Silicon) install the `.pkg` from the
  [releases page](https://github.com/fortunto2/superduper-dsp/releases); it puts both tools in `/usr/local/bin`. On other systems
  build from a checkout: `cargo build --release -p sdsp-chain` and `-p sdsp-bus`.
- **Needed by `sdsp-mash` and `superduper-plugin`:** a checkout of the repository and a Rust toolchain (`git clone https://github.com/fortunto2/superduper-dsp`).
- **Needed by `superduper-song` and `vocal-mix`:** REAPER 7+ and a REAPER MCP server connected to your agent. The plugin does not include one.
- **Python 3.11+** with `numpy` and `scipy` for the measurement scripts; `librosa`, `matplotlib`, `demucs` for `track-teardown`; `uv` for the `uvx` commands.
- `vocal-mix` mentions optional third-party UADx plugins; they are not required.

## What the plugin runs, sends and fetches

- **Reads:** audio files and TOML configs you point the agent at, and nothing else on disk. **Writes:** renders, PNGs and logs in the working
  folder you choose; the bus tools keep a small log at `~/.superduper-dsp/mix-log.jsonl`.
- **Runs locally:** Python scripts that the skills print as Markdown source (`skills/*/references/*-source.md`) for the agent to save and run in your working folder (analysis and optimisation of local WAV files); the plugin itself contains no executable files, plus the `sdsp-chain`, `sdsp-bus`
  and `sdsp-mash` programs you installed or built. The scripts make no network calls; they call `sdsp-chain` and `sdsp-bus` as subprocesses.
- **Fetches, only if you run those steps:** the skills tell the agent to run `uvx` / `uv run` for `muscriptor`, `basic-pitch` and `demucs` and
  to install `numpy` / `matplotlib` into a throwaway environment. That downloads packages from PyPI and model weights from their upstream hosts
  (MuScriptor's weights are gated on Hugging Face and need your own token, exported in your shell by you). Nothing is uploaded: your audio never leaves the machine.
- **No hooks, no MCP servers, no credentials** are included or requested by the plugin itself. `sdsp-bus` runs a local-only MCP server
  when you start it; that server is part of SuperDuper DSP, not of this plugin.

## Notes

- The skills are living documents written while making music on this project, all in English. Longer author logs that depend on a private setup
  (the `sdsp-mash` session log, a film-score recipe) are in the repository under `docs/skill-notes/`, not in the plugin.
  `audio-to-midi` and `track-teardown` describe a few of the author's own tools (SwiftF0 tracker, one-track scripts) as results and say when they are not bundled.
- Measurements quoted in the skills (speeds, accuracies) were taken on Apple Silicon.

## License

MIT, see [LICENSE](LICENSE). The plugins and tools are in the [main repository](https://github.com/fortunto2/superduper-dsp) under the same license.
