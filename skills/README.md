# SuperDuper DSP — Claude Code skills (source of truth)

These skills document/drive this repo. They live **here** (version-controlled with
the code) and are surfaced to Claude Code via **symlinks** from `~/.claude/skills/`
— no duplication, one source of truth.

| Skill | What it covers |
|---|---|
| `superduper-plugin` | Scaffold / modify a new CLAP plugin in this workspace (full checklist). |
| `superduper-song` | Make songs in REAPER driven by our instruments (Wave/Kubyz/Drum/Pad/Sampler) via the reaper MCP — param maps by name, preset-recall indices, raw-value gotcha, CC expression, audio→MIDI. |
| `sdsp-chain` | Headless mastering/mixing chain of our effects from the CLI (per-stage LUFS/dBTP). |
| `sdsp-mash` | Mashup / cypher engine (`tools/sdsp-mash`). Big living skill. |

## The symlink setup (recreate on a new machine)

```bash
REPO="/Users/rustam/Music/1music/superduper-dsp/skills"
for s in superduper-plugin superduper-song sdsp-chain sdsp-mash; do
  # if a real (non-symlink) copy exists globally, move it into the repo first:
  # mv "$HOME/.claude/skills/$s" "$REPO/$s"
  ln -sf "$REPO/$s" "$HOME/.claude/skills/$s"
done
```

## Rules
- **Edit the copy in this repo**, not the `~/.claude/skills/` symlink target — same file, but commit from here.
- These are **living skills**: append lessons/recipes after each session (each SKILL.md says where).
- `reaper-daw` stays global (generic DAW control, not tied to this repo). `superduper-song` links to it.
- Don't re-create global copies — that would duplicate. Check `readlink ~/.claude/skills/<name>` first.
