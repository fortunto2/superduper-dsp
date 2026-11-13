"""The Markdown source references in agent-skills/ must match the real scripts, and the plugin folder must hold only text types."""
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ALLOWED = {".md", ".txt", ".json", ".yaml", ".yml", ".png", ".jpg", ".jpeg", ".gif", ".webp", ""}


def test_references_match_sources():
    r = subprocess.run([sys.executable, str(ROOT / "scripts/build_skill_refs.py"), "--check"], capture_output=True, text=True)
    assert r.returncode == 0, r.stdout + r.stderr


def test_plugin_folder_is_text_only():
    for p in (ROOT / "agent-skills").rglob("*"):
        if p.is_file():
            assert p.suffix.lower() in ALLOWED, p
            assert not p.is_symlink(), p
            if p.suffix.lower() in {".md", ".txt", ".json"}:
                data = p.read_bytes()
                assert b"\0" not in data and not data.startswith(b"#!"), p
                data.decode("utf-8")
