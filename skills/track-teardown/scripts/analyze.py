#!/usr/bin/env python3
"""Look at a track the way you would listen to it: structure first, then detail.

Numbers alone lie about music. A tempo of 100.2 BPM tells you nothing about
whether the track feels like a march or a stagger. So this writes PNGs — the
agent reads them as images, which is the closest thing to hearing available
here: a mel spectrogram shows the kick pattern, the sidechain pumping, where
the bass enters, how long the tails are, and where the sections cut.
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
OUT.mkdir(exist_ok=True)

KRUMHANSL_MAJOR = np.array(
    [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88]
)
KRUMHANSL_MINOR = np.array(
    [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17]
)
NOTES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]


def estimate_key(y, sr):
    chroma = librosa.feature.chroma_cqt(y=y, sr=sr)
    profile = chroma.mean(axis=1)
    profile /= profile.sum()
    best = None
    for shift in range(12):
        rolled = np.roll(profile, -shift)
        for name, template in (("major", KRUMHANSL_MAJOR), ("minor", KRUMHANSL_MINOR)):
            score = np.corrcoef(rolled, template / template.sum())[0, 1]
            if best is None or score > best[0]:
                best = (score, NOTES[shift], name)
    return best[1], best[2], best[0], profile


def spectrogram_png(y, sr, path, title, fmax=16000, width=22, hop=512):
    """A mel spectrogram sized so features stay visible when read as an image."""
    mel = librosa.feature.melspectrogram(
        y=y, sr=sr, n_fft=2048, hop_length=hop, n_mels=160, fmax=fmax
    )
    db = librosa.power_to_db(mel, ref=np.max)
    fig, ax = plt.subplots(figsize=(width, 5.5), dpi=110)
    img = librosa.display.specshow(
        db, sr=sr, hop_length=hop, x_axis="time", y_axis="mel", fmax=fmax, ax=ax,
        cmap="magma", vmin=-70, vmax=0,
    )
    ax.set_title(title, fontsize=13)
    fig.colorbar(img, ax=ax, format="%+2.0f dB")
    fig.tight_layout()
    fig.savefig(path)
    plt.close(fig)


def band_energy_png(y, sr, path, title, beats_t=None):
    """Sub / low / mid / high over time — the arrangement map."""
    hop = 2048
    S = np.abs(librosa.stft(y, n_fft=4096, hop_length=hop))
    freqs = librosa.fft_frequencies(sr=sr, n_fft=4096)
    times = librosa.frames_to_time(np.arange(S.shape[1]), sr=sr, hop_length=hop)
    bands = {
        "sub 20-60": (20, 60),
        "bass 60-200": (60, 200),
        "low-mid 200-800": (200, 800),
        "mid 800-3k": (800, 3000),
        "high 3k-10k": (3000, 10000),
        "air 10k+": (10000, 20000),
    }
    fig, ax = plt.subplots(figsize=(22, 6), dpi=110)
    for label, (lo, hi) in bands.items():
        sel = (freqs >= lo) & (freqs < hi)
        e = 20 * np.log10(np.sqrt((S[sel] ** 2).mean(axis=0)) + 1e-9)
        ax.plot(times, e, label=label, linewidth=1.1)
    if beats_t is not None:
        for t in beats_t:
            ax.axvline(t, color="k", alpha=0.06, linewidth=0.5)
    ax.set_xlabel("time (s)")
    ax.set_ylabel("dB")
    ax.set_title(title, fontsize=13)
    ax.legend(loc="lower right", ncol=6, fontsize=9)
    ax.grid(alpha=0.2)
    ax.set_xlim(times[0], times[-1])
    fig.tight_layout()
    fig.savefig(path)
    plt.close(fig)


def main(src, tag):
    y, sr = librosa.load(src, sr=SR, mono=True)
    dur = len(y) / sr
    print(f"=== {tag} — {dur:.1f}s @ {sr} ===")

    tempo, beats = librosa.beat.beat_track(y=y, sr=sr, units="time", trim=False)
    tempo = float(np.atleast_1d(tempo)[0])
    print(f"tempo: {tempo:.2f} BPM   beats: {len(beats)}")
    if len(beats) > 2:
        ibi = np.diff(beats)
        print(f"beat interval: {ibi.mean()*1000:.1f} ms  (sd {ibi.std()*1000:.1f} ms)")
    print(f"first beat at {beats[0]:.3f}s" if len(beats) else "no beats")

    root, mode, conf, profile = estimate_key(y, sr)
    print(f"key: {root} {mode}  (confidence {conf:.2f})")
    order = np.argsort(profile)[::-1]
    print("pitch classes by energy: " + ", ".join(
        f"{NOTES[i]}={profile[i]*100:.1f}%" for i in order[:7]))

    rms = librosa.feature.rms(y=y, hop_length=2048)[0]
    rms_db = 20 * np.log10(rms + 1e-9)
    print(f"RMS: mean {rms_db.mean():.1f} dB  peak {rms_db.max():.1f} dB")
    peak = np.abs(y).max()
    print(f"peak {20*np.log10(peak+1e-12):.2f} dBFS  crest {20*np.log10(peak/ (np.sqrt((y**2).mean())+1e-12)):.1f} dB")

    spectrogram_png(y, sr, OUT / f"{tag}_full.png", f"{tag} — full ({dur:.0f}s)")
    band_energy_png(y, sr, OUT / f"{tag}_bands.png", f"{tag} — band energy", beats)

    np.save(OUT / f"{tag}_beats.npy", beats)
    return tempo, beats


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
