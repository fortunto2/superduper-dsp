#!/usr/bin/env python3
"""Read the kick and bass straight off the mix, not off a separated stem.

Demucs leaks — its drum stem carries distortion tails from the synths, and
librosa's onset detector fires on those, which is why the folded histogram came
back saying "all sixteen". A lowpassed envelope has no such problem: below
100 Hz the only things present are the kick and the bass fundamental, and their
peaks are unambiguous.
"""

from pathlib import Path

import librosa
import numpy as np
import scipy.signal as sig

SR = 44100
MIX = "/Users/rustam/Music/DJ/Incoming/Dark/Gesaffelstein - Hate or Glory.flac"
BPM = 103.36
BEAT = 60.0 / BPM
BAR = 4 * BEAT
T0 = 0.221
NAMES = ["1", "e", "&", "a", "2", "e", "&", "a",
         "3", "e", "&", "a", "4", "e", "&", "a"]


def envelope_hits(y, sr, cutoff, start, bars, prominence=0.15):
    a, b = start, start + bars * BAR
    seg = y[int(a * sr):int(b * sr)]
    sos = sig.butter(4, cutoff, "lowpass", fs=sr, output="sos")
    low = sig.sosfilt(sos, seg)
    env = np.abs(sig.hilbert(low))
    env = sig.sosfilt(sig.butter(2, 30, "lowpass", fs=sr, output="sos"), env)
    env /= env.max() + 1e-12
    peaks, props = sig.find_peaks(env, prominence=prominence, distance=int(0.05 * sr))
    return peaks / sr + a, props["prominences"], env, seg


def fold(times, weights, label, bars):
    counts = np.zeros(16)
    for t, w in zip(times, weights):
        slot = int(round(((t - T0) % BAR) / (BAR / 16))) % 16
        counts[slot] += w
    counts /= counts.max() + 1e-12
    print(f"\n{label}  ({len(times)} hits over {bars} bars = "
          f"{len(times)/bars:.1f} per bar)")
    line = []
    for i, c in enumerate(counts):
        hit = c > 0.28
        line.append(NAMES[i] if hit else ".")
        if c > 0.05:
            print(f"   {NAMES[i]:>2}  {c:4.2f} {'#' * int(c * 40)}")
    print(f"   → {' '.join(line)}")


def main():
    y, sr = librosa.load(MIX, sr=SR, mono=True)
    for label, start_bar, bars in [
        ("INTRO  bars 9-24", 8, 16),
        ("MAIN   bars 33-44", 32, 12),
        ("PEAK   bars 89-100", 88, 12),
    ]:
        start = T0 + start_bar * BAR
        t_k, w_k, _, _ = envelope_hits(y, sr, 90, start, bars, 0.18)
        fold(t_k, w_k, f"KICK  <90 Hz  {label}", bars)

    # Bass note length and pitch in the main section, measured on the mix.
    start = T0 + 32 * BAR
    seg = y[int(start * sr):int((start + 4 * BAR) * sr)]
    sos = sig.butter(6, 200, "lowpass", fs=sr, output="sos")
    low = sig.sosfilt(sos, seg)
    f0, voiced, _ = librosa.pyin(low, fmin=35, fmax=200, sr=sr,
                                 frame_length=8192, hop_length=256)
    good = f0[~np.isnan(f0)]
    if len(good):
        print(f"\nlow-end pitch in main section: median {np.median(good):.1f} Hz "
              f"= {librosa.hz_to_note(np.median(good))}, "
              f"range {np.percentile(good,5):.1f}-{np.percentile(good,95):.1f} Hz")

    # How long does the low end ring? Decay time from peak to -20 dB.
    t_k, _, env, _ = envelope_hits(y, sr, 120, start, 4, 0.18)
    if len(t_k) > 2:
        i0 = int((t_k[1] - start) * sr)
        tail = env[i0:i0 + int(0.6 * sr)]
        below = np.where(20 * np.log10(tail / (tail[0] + 1e-12) + 1e-12) < -20)[0]
        if len(below):
            print(f"low-end decay to -20 dB: {below[0]/sr*1000:.0f} ms "
                  f"({below[0]/sr/BEAT:.2f} beats)")


if __name__ == "__main__":
    main()
