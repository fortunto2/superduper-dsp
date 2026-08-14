#!/usr/bin/env python3
"""Measure a mix the way an engineer listens to one — and against a reference.

Every mix problem hit on this project so far was invisible until measured:
a kit playing with no hi-hats (20 dB hole above 2.5 kHz), a master whose three
drops sat within 0.1 dB of each other, a loudness meter reading 3 dB light. Ears
found them weeks later. These numbers find them in four seconds.

    python3 mixcheck.py mix.wav                       # absolute report
    python3 mixcheck.py mix.wav --ref commercial.wav  # A/B against a reference
    python3 mixcheck.py mix.wav --sections 140 8,16,32,40,56,64,76,80

What it reports and why:

  band balance   Energy per band relative to the loudest band. Compare against a
                 reference in the same genre — absolute targets are a myth, the
                 relationship between bands is not.
  crest factor   Peak over RMS. Under ~8 dB the mastering chain has flattened the
                 performance; 10-13 dB is a mix that still breathes.
  section arc    RMS per section. A build louder than its drop means the drop
                 never lands, however good the sounds are.
  correlation    Mono compatibility. Below 0 the sides cancel on a phone.
  low-end mono   Stereo width under 120 Hz — wide bass smears on club systems.
"""

import argparse
import cmath
import math
import struct
import sys
from pathlib import Path

BANDS = [
    (20, 60, "sub"),
    (60, 120, "kick"),
    (120, 250, "low"),
    (250, 800, "body"),
    (800, 2500, "mid"),
    (2500, 6000, "presence"),
    (6000, 16000, "air"),
]


def decode(path):
    """Minimal WAV reader — 16/24/32-bit int and 32-bit float, any channel count."""
    raw = Path(path).read_bytes()
    if raw[:4] != b"RIFF":
        raise SystemExit(f"{path}: not a RIFF file")
    i, fmt, ch, bits, sr, data = 12, 1, 2, 16, 44100, None
    while i + 8 <= len(raw):
        cid, size = raw[i:i + 4], int.from_bytes(raw[i + 4:i + 8], "little")
        body = raw[i + 8:i + 8 + size]
        if cid == b"fmt ":
            fmt = int.from_bytes(body[0:2], "little")
            ch = int.from_bytes(body[2:4], "little")
            sr = int.from_bytes(body[4:8], "little")
            bits = int.from_bytes(body[14:16], "little")
        elif cid == b"data":
            data = body
            break
        i += 8 + size + (size & 1)
    if data is None:
        raise SystemExit(f"{path}: no data chunk")

    n = len(data) // (ch * (bits // 8))
    left, right = [], []
    if fmt == 3 and bits == 32:
        vals = struct.unpack(f"<{n * ch}f", data[: n * ch * 4])
    elif bits == 16:
        vals = [v / 32768.0 for v in struct.unpack(f"<{n * ch}h", data[: n * ch * 2])]
    elif bits == 24:
        vals = []
        for k in range(n * ch):
            b = data[k * 3:k * 3 + 3]
            v = int.from_bytes(b, "little", signed=True)
            vals.append(v / 8388608.0)
    elif bits == 32:
        vals = [v / 2147483648.0 for v in struct.unpack(f"<{n * ch}i", data[: n * ch * 4])]
    else:
        raise SystemExit(f"{path}: unsupported format {fmt}/{bits}-bit")
    for k in range(n):
        left.append(vals[k * ch])
        right.append(vals[k * ch + 1] if ch > 1 else vals[k * ch])
    return left, right, float(sr)


def fft(x):
    m = len(x)
    if m <= 1:
        return x
    ev, od = fft(x[0::2]), fft(x[1::2])
    tw = [cmath.exp(-2j * math.pi * k / m) * od[k] for k in range(m // 2)]
    return [ev[k] + tw[k] for k in range(m // 2)] + [ev[k] - tw[k] for k in range(m // 2)]


def band_energy(mono, sr, n_fft=4096, hop_frames=8):
    win = [0.5 - 0.5 * math.cos(2 * math.pi * i / (n_fft - 1)) for i in range(n_fft)]
    acc = {b[2]: 0.0 for b in BANDS}
    step = n_fft * hop_frames
    for s in range(0, max(1, len(mono) - n_fft), step):
        spec = fft([complex(mono[s + i] * win[i], 0) for i in range(n_fft)])
        for lo, hi, name in BANDS:
            i0, i1 = int(lo * n_fft / sr), int(hi * n_fft / sr)
            acc[name] += sum(abs(spec[i]) ** 2 for i in range(i0, min(i1 + 1, n_fft // 2)))
    ref = max(acc.values()) or 1.0
    return {k: 10 * math.log10(v / ref + 1e-12) for k, v in acc.items()}


def db(x):
    return 20 * math.log10(abs(x) + 1e-12)


def rms(seg):
    return (sum(v * v for v in seg) / max(1, len(seg))) ** 0.5


def report(path, sections=None, ref=None):
    l, r, sr = decode(path)
    n = min(len(l), len(r))
    mono = [(l[i] + r[i]) * 0.5 for i in range(n)]
    side = [(l[i] - r[i]) * 0.5 for i in range(n)]

    peak = max(max(abs(v) for v in l), max(abs(v) for v in r))
    overall = rms(mono)
    crest = db(peak) - db(overall)
    print(f"\n── {Path(path).name}   {n / sr:.1f} s @ {sr:.0f} Hz")
    print(f"   peak {db(peak):+6.2f} dBFS    rms {db(overall):+6.2f} dB    crest {crest:5.1f} dB", end="")
    if crest < 8:
        print("   ← squashed; the chain is flattening the performance")
    elif crest > 14:
        print("   ← very open; fine, unless it feels quiet next to the reference")
    else:
        print()

    # Mono compatibility: correlation between channels, and how wide the low end is.
    num = sum(l[i] * r[i] for i in range(n))
    den = math.sqrt(sum(v * v for v in l) * sum(v * v for v in r)) or 1.0
    corr = num / den
    low_side = rms([side[i] for i in range(0, n, 4)])
    low_mid = rms([mono[i] for i in range(0, n, 4)])
    print(f"   correlation {corr:+.2f}" + ("   ← sides cancel in mono" if corr < 0.2 else ""))
    print(f"   side/mid {db(low_side) - db(low_mid):+.1f} dB")

    mine = band_energy(mono, sr)
    theirs = band_energy(decode(ref)[0], decode(ref)[2]) if ref else None
    if ref:
        rl, rr, rsr = decode(ref)
        theirs = band_energy([(rl[i] + rr[i]) * 0.5 for i in range(min(len(rl), len(rr)))], rsr)

    print("\n   band            this mix" + ("      reference    delta" if ref else ""))
    for lo, hi, name in BANDS:
        line = f"   {name:9s} {lo:5d}-{hi:<5d} {mine[name]:6.1f} dB"
        if ref:
            d = mine[name] - theirs[name]
            flag = "  ←" if abs(d) >= 4 else ""
            line += f"   {theirs[name]:6.1f} dB   {d:+5.1f}{flag}"
        print(line)
    if ref:
        print("   (delta ≥ 4 dB is audible as 'darker' / 'harsher' than the reference)")

    if sections:
        bpm, bars = sections
        bar_s = 60.0 / bpm * 4
        print(f"\n   section arc @ {bpm:.0f} BPM")
        prev_name, prev_db = None, None
        for i, (b0, b1, name) in enumerate(bars):
            i0, i1 = int(b0 * bar_s * sr), min(int(b1 * bar_s * sr), n)
            d = db(rms(mono[i0:i1]))
            arrow = ""
            if prev_db is not None:
                arrow = f"   {d - prev_db:+5.1f} dB vs {prev_name}"
            print(f"   {name:12s} bars {b0:3d}-{b1:<3d} {d:6.1f} dB {'█' * max(0, int(62 + d))}{arrow}")
            prev_name, prev_db = name, d


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("mix")
    ap.add_argument("--ref", help="commercial reference to compare band balance against")
    ap.add_argument("--bpm", type=float, help="enable the section arc")
    ap.add_argument("--bars", help="comma-separated bar boundaries, e.g. 8,16,32,40,56,64,76,80")
    a = ap.parse_args()

    sections = None
    if a.bpm and a.bars:
        edges = [0] + [int(x) for x in a.bars.split(",")]
        sections = (a.bpm, [(edges[i], edges[i + 1], f"bars {edges[i]}-{edges[i+1]}")
                            for i in range(len(edges) - 1)])
    report(a.mix, sections, a.ref)


if __name__ == "__main__":
    main()
