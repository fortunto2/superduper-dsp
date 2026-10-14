"""Instrument presets, paths and seeds — from instruments.toml, not from code."""

import os
import shutil
import tomllib
from functools import cache
from pathlib import Path

DEFAULT = Path(__file__).with_name("instruments.toml")


@cache
def load(path: str | None = None) -> dict:
    p = Path(path or os.environ.get("SDSP_SCORE_INSTRUMENTS") or DEFAULT)
    return tomllib.loads(p.read_text())


def cfg() -> dict:
    return load(os.environ.get("SDSP_SCORE_INSTRUMENTS"))


def samples_dir() -> Path:
    return Path(os.path.expanduser(os.environ.get("SDSP_SAMPLES") or cfg()["samples_dir"]))


def ffmpeg(full: bool = False) -> str:
    env = os.environ.get("SDSP_FFMPEG")
    if env:
        return env
    if full and Path(cfg()["ffmpeg_full"]).exists():
        return cfg()["ffmpeg_full"]
    return shutil.which(cfg()["ffmpeg"]) or cfg()["ffmpeg"]


def chain_bin() -> str:
    """$SDSP_CHAIN, then `sdsp-chain` on PATH, then the configured cargo target."""
    return os.environ.get("SDSP_CHAIN") or shutil.which("sdsp-chain") or os.path.expanduser(cfg()["chain"])
