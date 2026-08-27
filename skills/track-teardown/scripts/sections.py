#!/usr/bin/env python3
"""Per-section groove, so the histogram shows a pattern instead of a smear.

Folding a whole 289 s track into one bar fills every 1/16 slot — the song
passes through several grooves. This cuts it into 16-bar windows first.
"""

from pathlib import Path

import librosa
import numpy as np

from groove import BAR, BEAT, STEMS, T0, grid_histogram, onsets_of

SR = 44100
MIX = "/Users/rustam/Music/DJ/Incoming/Dark/Gesaffelstein - Hate or Glory.flac"


def arrangement():
    """Level per 4 bars — where things enter, drop out, and break."""
    y, sr = librosa.load(MIX, sr=SR, mono=True)
    print("bar   time    total   sub    lowmid  high   what")
    S = np.abs(librosa.stft(y, n_fft=4096, hop_length=2048))
    freqs = librosa.fft_frequencies(sr=sr, n_fft=4096)
    times = librosa.frames_to_time(np.arange(S.shape[1]), sr=sr, hop_length=2048)

    def band(lo, hi):
        sel = (freqs >= lo) & (freqs < hi)
        return 20 * np.log10(np.sqrt((S[sel] ** 2).mean(axis=0)) + 1e-9)

    tot, sub, lm, hi_ = band(20, 16000), band(20, 120), band(200, 2500), band(3000, 12000)
    n_bars = int((len(y) / sr - T0) / BAR)
    for b in range(0, n_bars, 4):
        t0, t1 = T0 + b * BAR, T0 + (b + 4) * BAR
        m = (times >= t0) & (times < t1)
        if not m.any():
            continue
        row = (tot[m].mean(), sub[m].mean(), lm[m].mean(), hi_[m].mean())
        # A break reads as the total dropping well below the running level.
        print(f"{b+1:4d} {t0:6.1f}s  {row[0]:6.1f} {row[1]:6.1f} {row[2]:7.1f} {row[3]:6.1f}")


def per_section_groove():
    windows = [
        ("intro     bars 9-24", T0 + 8 * BAR, T0 + 24 * BAR),
        ("main      bars 33-48", T0 + 32 * BAR, T0 + 48 * BAR),
        ("late      bars 89-104", T0 + 88 * BAR, T0 + 104 * BAR),
    ]
    for name, stem, lo, hi in [
        ("kick", "drums.wav", None, 150),
        ("hats", "drums.wav", 4000, None),
        ("bass", "bass.wav", None, 300),
    ]:
        on = onsets_of(STEMS / stem, f"{name}", lo=lo, hi=hi)
        for label, a, b in windows:
            grid_histogram(on, f"{name.upper()} — {label}", window=(a, b))


if __name__ == "__main__":
    import sys
    if len(sys.argv) > 1 and sys.argv[1] == "arr":
        arrangement()
    else:
        per_section_groove()
