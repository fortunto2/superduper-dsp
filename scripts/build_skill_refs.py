"""Generate the Markdown source references that ship in agent-skills/ from the real scripts.

The plugin directory accepts only text-type files, so a skill cannot carry `.py` files. Each
script a skill needs is shipped as `skills/<skill>/references/<name>-source.md` (a fenced python
block with the shebang removed). The scripts under tools/ stay the single source of truth.

    python3 scripts/build_skill_refs.py          # rewrite the references
    python3 scripts/build_skill_refs.py --check  # exit 1 if any reference is stale (used by tools/test_skill_refs.py)
"""
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FENCE = "`" * 4

# skill, tool path, one-line purpose, dependencies
SCRIPTS = [
    ("sdsp-mix", "tools/mixcheck.py", "Measure a finished mix: LUFS, crest, band balance against a reference, stereo correlation, section arc.",
     "Python 3.11+; standard library for the basics, `numpy` and `scipy` for band and mono checks."),
    ("sdsp-mix", "tools/mixverify.py", "Keep a mix change only if two instruments (offline render and live bus log) agree. Imports `mixcheck`; save both in one folder.",
     "Python 3.11+, `numpy`; the `sdsp-chain` binary for renders."),
    ("sdsp-mix", "tools/mixopt.py", "Bounded Nelder-Mead optimiser over a whitelist of chain parameters against a measured loss. Imports `mixcheck`; save both in one folder.",
     "Python 3.11+, `numpy`, `scipy`; the `sdsp-chain` binary."),
    ("sdsp-mix", "tools/sdsp-bus/mixmap.py", "Record the live SuperDuper bus and draw who sits where in the spectrum and who masks whom.",
     "`uv` (inline script metadata installs `numpy`, `matplotlib`); the `sdsp-bus` binary."),
    ("audio-to-midi", "tools/skill-scripts/audio-to-midi/gridcheck.py", "Check whether MIDI onsets sit on the beat grid, with deviation in ms and every missed beat.",
     "`mido` (for example `uvx --with mido python gridcheck.py ...`)."),
    ("audio-to-midi", "tools/skill-scripts/audio-to-midi/midinspect.py", "Print per-track note counts, ranges, pitch-class histograms and duration medians of a MIDI file.",
     "`mido`."),
    ("track-teardown", "tools/skill-scripts/track-teardown/analyze.py", "Write tempo, key, spectrogram and band PNGs for a track to ./ref so an agent can read them as images.",
     "`numpy`, `librosa`, `matplotlib`."),
    ("track-teardown", "tools/skill-scripts/track-teardown/zoom.py", "Write a close-up spectrogram and waveform of a few bars to ./ref.",
     "`numpy`, `librosa`, `matplotlib`."),
]


def render(skill: str, tool: str, purpose: str, deps: str) -> tuple[Path, str]:
    src = (ROOT / tool).read_text(encoding="utf-8")
    lines = src.split("\n")
    if lines and lines[0].startswith("#!"):
        lines = lines[1:]
    code = "\n".join(lines).rstrip("\n") + "\n"
    assert FENCE not in code, f"{tool} contains a four-backtick run"
    name = Path(tool).name
    body = (
        f"# {name} (source)\n\n"
        f"{purpose} Dependencies: {deps}\n\n"
        f"To use it: save the block below as `{name}` in your working folder, then run it with the command shown in the skill. "
        f"If a repository checkout or the installed SuperDuper DSP tools already provide it, run that copy instead.\n\n"
        f"{FENCE}python\n{code}{FENCE}\n"
    )
    out = ROOT / "agent-skills" / "skills" / skill / "references" / f"{Path(tool).stem}-source.md"
    return out, body


def main() -> int:
    stale = []
    for entry in SCRIPTS:
        out, body = render(*entry)
        if "--check" in sys.argv:
            if not out.exists() or out.read_text(encoding="utf-8") != body:
                stale.append(str(out.relative_to(ROOT)))
        else:
            out.parent.mkdir(parents=True, exist_ok=True)
            out.write_text(body, encoding="utf-8")
    if stale:
        print("stale references (run scripts/build_skill_refs.py):", *stale, sep="\n  ")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
