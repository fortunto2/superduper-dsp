#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy", "matplotlib"]
# ///
"""Mix map from the SuperDuper bus: who sits where in the spectrum, and who
fights whom.

    uv run mixmap.py                 # record 20 s of playback, write mixmap.png
    uv run mixmap.py -s 40 -o x.png

Reads `sdsp-bus --json` twice a second while the song plays. Per track it
keeps the LOUDEST instance on that track (the bus has no plugin order, and
the loudest is usually the track's output stage). Three panels:

  1. average spectrum of every track, one colour each (dB, band RMS);
  2. masking: for each pair of tracks, the bands where both are loud
     (> -50 dB) and within 6 dB of each other, weighted by how often;
  3. a spectrogram (32 bands x time) per track.

Band levels are band RMS, so a track's bands sum to its level.
"""
import argparse
import json
import subprocess
import time
from pathlib import Path

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt  # noqa: E402
import numpy as np  # noqa: E402

BUS = Path.home() / ".cargo-target/release/sdsp-bus"
LOUD = -50.0
CLOSE = 6.0


def snapshot():
    out = subprocess.run([str(BUS), "--json"], capture_output=True, text=True, check=True).stdout
    return json.loads(out)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-s", "--seconds", type=float, default=20.0)
    ap.add_argument("-o", "--out", default="mixmap.png")
    ap.add_argument("--min-db", type=float, default=-70.0, help="ignore tracks quieter than this on average")
    a = ap.parse_args()

    frames = []  # list of {track: bands}
    edges = None
    t_end = time.time() + a.seconds
    while time.time() < t_end:
        rows = snapshot()
        per = {}
        for r in rows:
            name = r["track"] or f"(no name) {r['plugin'].removeprefix('SuperDuper ')}"
            edges = r["band_edges_hz"]
            if name not in per or r["rms_db"] > per[name][0]:
                per[name] = (r["rms_db"], np.array(r["bands_db"]))
        frames.append({k: v[1] for k, v in per.items()})
        time.sleep(0.5)
    if not frames or edges is None:
        raise SystemExit("bus is empty: is REAPER playing with SuperDuper plugins?")

    tracks = sorted({k for f in frames for k in f})
    T = len(frames)
    B = len(edges) - 1
    data = {k: np.array([f.get(k, np.full(B, -120.0)) for f in frames]) for k in tracks}
    avg = {k: 10 * np.log10(np.mean(10 ** (v / 10), axis=0) + 1e-12) for k, v in data.items()}
    total = {k: 10 * np.log10(np.sum(10 ** (avg[k] / 10)) + 1e-12) for k in tracks}
    tracks = [k for k in tracks if total[k] > a.min_db]
    if not tracks:
        raise SystemExit("every track is silent: start playback first")
    centers = np.sqrt(np.array(edges[:-1]) * np.array(edges[1:]))

    # The master (and any unnamed bus) carries every track, so it "clashes"
    # with all of them and with its own other stages. Masking is between
    # real tracks only.
    is_bus = lambda k: k.startswith("(no name)") or k.lower() in ("master", "мастер")  # noqa: E731
    real = [k for k in tracks if not is_bus(k)]
    pairs = [(x, y) for i, x in enumerate(real) for y in real[i + 1:]]
    mask = np.zeros((max(len(pairs), 1), B))
    for pi, (x, y) in enumerate(pairs):
        dx, dy = data[x], data[y]
        clash = (dx > LOUD) & (dy > LOUD) & (np.abs(dx - dy) < CLOSE)
        mask[pi] = clash.mean(axis=0)
    order = np.argsort(-mask.sum(axis=1))[:12]

    n_spec = len(tracks)
    fig = plt.figure(figsize=(14, 5 + 2.2 + 1.1 * n_spec), constrained_layout=True)
    gs = fig.add_gridspec(2 + n_spec, 1, height_ratios=[4, 2.2] + [1.1] * n_spec)

    ax = fig.add_subplot(gs[0])
    for k in tracks:
        ax.semilogx(centers, avg[k], lw=2, label=f"{k}  ({total[k]:.1f} dB)")
    ax.set_xlim(20, 20000)
    ax.set_ylim(-90, max(-10, max(np.max(v) for v in avg.values()) + 3))
    ax.set_ylabel("band RMS, dB")
    ax.set_title(f"Track spectra — average over {T * 0.5:.0f} s of playback")
    ax.grid(True, which="both", alpha=0.25)
    ax.legend(fontsize=8, ncol=2, loc="lower left")

    ax = fig.add_subplot(gs[1])
    if len(order):
        ax.imshow(mask[order], aspect="auto", cmap="magma", vmin=0, vmax=1,
                  extent=[0, B, len(order), 0])
        ax.set_yticks(np.arange(len(order)) + 0.5)
        ax.set_yticklabels([f"{pairs[i][0]} × {pairs[i][1]}" for i in order], fontsize=7)
    ax.set_xticks(np.arange(0, B + 1, 4))
    ax.set_xticklabels([f"{e:.0f}" for e in edges[::4]], fontsize=7)
    ax.set_title(f"Masking — share of time both are > {LOUD:.0f} dB and within {CLOSE:.0f} dB of each other")

    for i, k in enumerate(tracks):
        ax = fig.add_subplot(gs[2 + i])
        ax.imshow(data[k].T, origin="lower", aspect="auto", cmap="viridis", vmin=-90, vmax=-10,
                  extent=[0, T * 0.5, 0, B])
        ax.set_yticks([0, 8, 16, 24, 32])
        ax.set_yticklabels([f"{edges[j]:.0f}" for j in [0, 8, 16, 24, 32]], fontsize=6)
        ax.set_ylabel(k[:22], fontsize=7, rotation=0, ha="right")
        if i < n_spec - 1:
            ax.set_xticks([])
    fig.axes[-1].set_xlabel("seconds")
    fig.savefig(a.out, dpi=110)
    print(f"wrote {a.out}: {len(tracks)} tracks, {T} frames")
    for i in order[:5]:
        if mask[i].max() > 0.2:
            b = int(np.argmax(mask[i]))
            print(f"  clash {pairs[i][0]} × {pairs[i][1]}: worst at {edges[b]:.0f}-{edges[b + 1]:.0f} Hz ({mask[i][b] * 100:.0f}% of the time)")


if __name__ == "__main__":
    main()
