# SuperDuper DSP — agent skills (source of truth)

These skills document and drive this repo. They live **here**, versioned with the code, and reach
agents by one of three routes below. No copies anywhere else.

| Skill | What it covers |
|---|---|
| `sdsp-mix` | Mixing and mastering by measurement: sidechain, band balance against a reference, `tools/mixcheck.py`. |
| `superduper-song` | Songs in REAPER on our instruments (Wave/Kubyz/Drum/Pad/Sampler) via the REAPER MCP: param maps by name, preset indices, CC expression, audio→MIDI. |
| `sdsp-chain` | Headless mastering and mixing chains of our effects from the CLI, LUFS and dBTP per stage. |
| `audio-to-midi` | A recording to MIDI, checked before it is trusted. |
| `sdsp-mash` | Mashup and cypher engine (`tools/sdsp-mash`). |
| `track-teardown` | Reverse-engineer a reference track (spectrograms, demucs stems, fitted tempo, arrangement dynamics) and converge a rebuild onto it numerically. |
| `vocal-mix` | A vocal chain, slot by slot. |
| `mic-compare` | Compare microphones on the same take, by measurement. |
| `superduper-plugin` | Scaffold or modify a CLAP plugin in this workspace (full checklist). |

## Install — pick one route

**Claude Code plugin** (updates with the repo):

```bash
claude plugin marketplace add https://github.com/fortunto2/superduper-dsp
claude plugin install superduper-dsp@superduper-dsp --scope user
```

**Other agents** (Cursor, Codex, Gemini CLI):

```bash
npx skills add fortunto2/superduper-dsp --all
```

**With the plugins installer**: `sdsp-bus setup` links `sdsp-mix`, `superduper-song`, `sdsp-chain`
and `audio-to-midi` into `~/.claude/skills` and `~/.codex/skills`. If you also install the Claude
Code plugin, the same skill is reachable twice; keep one route (`sdsp-bus setup --uninstall` takes
the links back).

**Working on this repo**: link your checkout instead, so edits are live:

```bash
REPO="$(git rev-parse --show-toplevel)/skills"
for s in "$REPO"/*/; do ln -sfn "$s" "$HOME/.claude/skills/$(basename "$s")"; done
```

## Rules
- **Edit the copy in this repo**, never an installed one. A copy forks silently.
- These are **living skills**: append lessons and recipes after each session (each SKILL.md says where).
- `reaper-daw` stays global (generic DAW control, not tied to this repo). `superduper-song` links to it.
- One route per skill. Check `readlink ~/.claude/skills/<name>` before adding another.
