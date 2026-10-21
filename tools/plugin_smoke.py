#!/usr/bin/env python3
"""Render a test signal through every plugin sdsp-chain knows, headless, and
check the output is sane — the CLI smoke test before a release.

    python3 tools/plugin_smoke.py                 # every effect, defaults + extra cases
    python3 tools/plugin_smoke.py --only multiband,compressor

Per render: no NaN/inf, not silent, peak under +12 dBFS, and the level change
against the input (a plugin that is "off by default" should read ~0 dB).
Exit 1 if any render fails. Needs numpy; uses the sdsp-chain binary from
$CARGO_TARGET_DIR (or ./target) — build it first: cargo build --release -p sdsp-chain.
"""
import argparse
import math
import os
import struct
import subprocess
import sys
import tempfile
import wave
from pathlib import Path

import numpy as np

SR = 48000

# Cases beyond defaults: the modes added on bus-mixmap, so the CLI exercises them.
EXTRA = {
    "compressor": [{"Detector": 1.0, "Threshold": -30.0}],
    "saturator": [{"Type": 3.0, "Curve": 20.0, "Drive": 6.0}],
    "eq": [{"Dyn Source": 1.0, "Dyn Gain": -9.0}],
    "multiband": [{"Bands": 4.0}],
}


# Instruments that sit in the effects list but only sound with MIDI notes
# (sdsp-chain renders them with `notes` on a [[track]]): silence is correct.
SILENT_WITHOUT_NOTES = {"wind"}


def chain_bin():
    root = Path(os.environ.get("CARGO_TARGET_DIR", Path(__file__).resolve().parents[1] / "target"))
    b = root / "release" / "sdsp-chain"
    if not b.exists():
        sys.exit(f"{b} missing: cargo build --release -p sdsp-chain")
    return str(b)


def write_input(path, seconds=3.0):
    """A small mix: 55 Hz bass, a 220 Hz saw-ish lead, and noise bursts — low,
    tonal, transient and broadband content for every kind of plugin."""
    n = int(seconds * SR)
    t = np.arange(n) / SR
    bass = 0.3 * np.sin(2 * math.pi * 55 * t)
    lead = sum(0.15 / k * np.sin(2 * math.pi * 220 * k * t) for k in range(1, 12))
    rng = np.random.default_rng(7)
    bursts = 0.2 * rng.standard_normal(n) * (np.sin(2 * math.pi * 2 * t) > 0.6)
    l, r = bass + lead + bursts, bass + 0.9 * lead + bursts[::-1]
    with wave.open(str(path), "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(SR)
        st = np.clip(np.stack([l, r], 1), -1, 1)
        w.writeframes((st * 32767).astype("<i2").tobytes())
    return np.stack([l, r])


def read_wav(path):
    raw = Path(path).read_bytes()
    i, fmt, ch, bits = 12, 1, 2, 16
    while i + 8 <= len(raw):
        cid, size = raw[i:i + 4], int.from_bytes(raw[i + 4:i + 8], "little")
        body = raw[i + 8:i + 8 + size]
        if cid == b"fmt ":
            fmt, ch = int.from_bytes(body[0:2], "little"), int.from_bytes(body[2:4], "little")
            bits = int.from_bytes(body[14:16], "little")
        elif cid == b"data":
            if fmt == 3:
                x = np.frombuffer(body, "<f4")
            elif bits == 16:
                x = np.frombuffer(body, "<i2") / 32768.0
            elif bits == 24:
                b = np.frombuffer(body[: len(body) // 3 * 3], np.uint8).reshape(-1, 3)
                x = (b[:, 0].astype(np.int32) | b[:, 1].astype(np.int32) << 8 | b[:, 2].astype(np.int32) << 16)
                x = np.where(x >= 1 << 23, x - (1 << 24), x) / float(1 << 23)
            else:
                x = np.frombuffer(body, "<i4") / 2147483648.0
            return x.reshape(-1, ch).T
        i += 8 + size + (size & 1)
    raise ValueError(f"{path}: no data")


def db(x):
    return 20 * math.log10(max(float(x), 1e-12))


def effects(binary):
    out = subprocess.run([binary, "--list"], capture_output=True, text=True, check=True).stdout
    keys, in_fx = [], False
    for line in out.splitlines():
        if line.startswith("Renderable effects"):
            in_fx = True
            continue
        if in_fx and not line.startswith(" "):
            break
        if in_fx and "params" in line:
            keys.append(line.split()[0])
    return keys


def render(binary, key, params, src, dst, tmp):
    cfg = Path(tmp) / f"{key}.toml"
    body = f'[[stage]]\nplugin = "{key}"\n'
    if params:
        body += "params = { " + ", ".join(f'"{k}" = {v}' for k, v in params.items()) + " }\n"
    cfg.write_text(body)
    p = subprocess.run([binary, str(cfg), str(src), str(dst)], capture_output=True, text=True, timeout=300)
    return p.returncode, (p.stderr or p.stdout).strip().splitlines()[-1:] if p.returncode else []


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--only", help="comma-separated plugin keys")
    a = ap.parse_args()
    binary = chain_bin()
    keys = effects(binary)
    if a.only:
        keys = [k for k in keys if k in a.only.split(",")]
    fails = 0
    with tempfile.TemporaryDirectory() as tmp:
        src = Path(tmp) / "in.wav"
        x = write_input(src)
        in_rms = db(np.sqrt((x ** 2).mean()))
        print(f"{'plugin':12s} {'case':28s} {'peak':>8s} {'rms Δ':>8s}  result")
        for key in keys:
            for params in [{}] + EXTRA.get(key, []):
                dst = Path(tmp) / f"{key}.wav"
                code, err = render(binary, key, params, src, dst, tmp)
                case = ",".join(f"{k}={v:g}" for k, v in params.items()) or "defaults"
                if code:
                    print(f"{key:12s} {case:28s} {'':>8s} {'':>8s}  FAIL render: {' '.join(err)}")
                    fails += 1
                    continue
                y = read_wav(dst)
                problems = []
                if not np.isfinite(y).all():
                    problems.append("NaN/inf")
                peak = db(np.abs(y).max())
                rms = db(np.sqrt((y ** 2).mean()))
                if rms < -80 and key not in SILENT_WITHOUT_NOTES:
                    problems.append("silent")
                if peak > 12:
                    problems.append("peak > +12 dBFS")
                print(f"{key:12s} {case:28s} {peak:+7.1f} {rms - in_rms:+7.1f}   " + ("FAIL " + ", ".join(problems) if problems else "ok"))
                fails += bool(problems)
    print(f"\n{len(keys)} plugins, {fails} failing")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
