#!/usr/bin/env python3
"""Pin the tempo down to the third decimal, and find the true downbeat.

librosa reported 103.36 BPM. Over 88 bars a 0.3 BPM error accumulates to more
than a beat, which is exactly what showed up: the kick appeared on a different
1/16 in each section. That is drift, not groove. Fit the tempo to the kick
times over the whole track instead of trusting a single estimate.
"""

import librosa
import numpy as np
import scipy.signal as sig

SR = 44100
MIX = "/Users/rustam/Music/DJ/Incoming/Dark/Gesaffelstein - Hate or Glory.flac"


def kick_times(y, sr, cutoff=90, prominence=0.20):
    sos = sig.butter(4, cutoff, "lowpass", fs=sr, output="sos")
    low = sig.sosfilt(sos, y)
    env = np.abs(sig.hilbert(low))
    env = sig.sosfilt(sig.butter(2, 25, "lowpass", fs=sr, output="sos"), env)
    env /= env.max() + 1e-12
    peaks, props = sig.find_peaks(env, prominence=prominence,
                                  distance=int(0.12 * sr))
    return peaks / sr, props["prominences"]


def best_grid(times, weights, bpm_lo=95, bpm_hi=115, steps=4001):
    """Which (bpm, phase) puts the most hit energy on a 1/16 line?"""
    best = None
    for bpm in np.linspace(bpm_lo, bpm_hi, steps):
        step = 60.0 / bpm / 4          # a 1/16
        frac = (times / step) % 1.0
        # circular mean: how tightly do the hits cluster on the grid?
        ang = 2 * np.pi * frac
        r = np.abs((weights * np.exp(1j * ang)).sum()) / weights.sum()
        if best is None or r > best[0]:
            phase = (np.angle((weights * np.exp(1j * ang)).sum()) / (2 * np.pi)) % 1.0
            best = (r, bpm, phase * step)
    return best


def main():
    y, sr = librosa.load(MIX, sr=SR, mono=True)
    t, w = kick_times(y, sr)
    print(f"{len(t)} low-end hits detected")

    r, bpm, off = best_grid(t, w)
    print(f"\nbest fit: {bpm:.3f} BPM   grid offset {off*1000:.1f} ms   "
          f"tightness {r:.3f} (1.0 = every hit dead on a 1/16)")

    beat = 60.0 / bpm
    bar = 4 * beat
    print(f"beat {beat*1000:.2f} ms   bar {bar:.4f} s")

    # Now place the downbeat: try all 16 rotations, pick the one where the
    # strongest recurring hit lands on a musically sensible slot.
    names = ["1", "e", "&", "a", "2", "e", "&", "a",
             "3", "e", "&", "a", "4", "e", "&", "a"]
    for label, a, b in [("intro", 18, 55), ("main", 74, 102),
                        ("peak", 204, 232), ("late", 260, 279)]:
        m = (t >= a) & (t < b)
        counts = np.zeros(16)
        for ti, wi in zip(t[m], w[m]):
            slot = int(round(((ti - off) % bar) / (bar / 16))) % 16
            counts[slot] += wi
        counts /= counts.max() + 1e-12
        line = " ".join(names[i] if counts[i] > 0.3 else "." for i in range(16))
        per_bar = m.sum() / ((b - a) / bar)
        print(f"  {label:6s} {per_bar:4.1f} hits/bar   {line}")

    print("\nSlot 0 here is the grid's own phase, not necessarily the downbeat — "
          "read the pattern shape, not the labels.")


if __name__ == "__main__":
    main()
