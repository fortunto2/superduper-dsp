#!/usr/bin/env python3
"""Close-ups. The full-track view shows structure; this shows the groove.

Eight bars wide, so every hit is a distinguishable vertical mark and the
pattern can be read off the picture instead of guessed from a BPM number.
"""

import sys
from pathlib import Path

import librosa
import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np

SR = 44100
OUT = Path(__file__).parent / "ref"


def zoom(src, tag, start, bars, bpm, fmax=16000, n_mels=200):
    bar = 4 * 60.0 / bpm
    dur = bars * bar
    y, sr = librosa.load(src, sr=SR, mono=True, offset=start, duration=dur)
    hop = 256
    mel = librosa.feature.melspectrogram(
        y=y, sr=sr, n_fft=2048, hop_length=hop, n_mels=n_mels, fmax=fmax
    )
    db = librosa.power_to_db(mel, ref=np.max)

    fig, (ax, axw) = plt.subplots(
        2, 1, figsize=(24, 8), dpi=110, gridspec_kw={"height_ratios": [3, 1]},
        sharex=True,
    )
    img = librosa.display.specshow(
        db, sr=sr, hop_length=hop, x_axis="time", y_axis="mel", fmax=fmax,
        ax=ax, cmap="magma", vmin=-70, vmax=0,
    )
    # Beat and bar grid, so hits can be placed on the 1/8 and 1/16 lines.
    for b in np.arange(0, dur, bar / 4):
        ax.axvline(b, color="cyan", alpha=0.35, linewidth=0.7)
        axw.axvline(b, color="cyan", alpha=0.35, linewidth=0.7)
    for b in np.arange(0, dur, bar):
        ax.axvline(b, color="white", alpha=0.9, linewidth=1.4)
        axw.axvline(b, color="white", alpha=0.9, linewidth=1.4)
    for b in np.arange(0, dur, bar / 8):
        ax.axvline(b, color="cyan", alpha=0.12, linewidth=0.4)
    ax.set_title(f"{tag} — {bars} bars from {start:.2f}s @ {bpm:.2f} BPM "
                 f"(white=bar, cyan=beat, faint=1/8)", fontsize=13)
    ax.set_ylabel("Hz")

    t = np.arange(len(y)) / sr
    axw.plot(t, y, linewidth=0.4, color="black")
    axw.set_xlim(0, dur)
    axw.set_ylabel("wave")
    axw.grid(alpha=0.2)
    fig.tight_layout()
    path = OUT / f"zoom_{tag}.png"
    fig.savefig(path)
    plt.close(fig)
    print(f"wrote {path}")


if __name__ == "__main__":
    src, tag, start, bars, bpm = sys.argv[1:6]
    fmax = int(sys.argv[6]) if len(sys.argv) > 6 else 16000
    zoom(src, tag, float(start), int(bars), float(bpm), fmax)
