#!/usr/bin/env python3
"""Measure a mix the way an engineer listens to one — and against a reference.

Every mix problem hit on this project so far was invisible until measured:
a kit playing with no hi-hats (20 dB hole above 2.5 kHz), a master whose three
drops sat within 0.1 dB of each other, a loudness meter reading 3 dB light. Ears
found them weeks later. These numbers find them in four seconds.

    python3 mixcheck.py mix.wav                       # absolute report
    python3 mixcheck.py mix.wav --ref commercial.wav  # A/B against a reference
    python3 mixcheck.py mix.wav --sections 140 8,16,32,40,56,64,76,80
    python3 mixcheck.py mix.wav --start 62 --end 78 --ref ref.wav --ref-start 55 --ref-end 71
    python3 mixcheck.py ref.wav --start 55 --end 71 --json > ref-chorus.json   # a profile

What it reports and why:

  band balance   Energy per band relative to the loudest band. Compare against a
                 reference in the same genre — absolute targets are a myth, the
                 relationship between bands is not.
  crest factor   Peak over RMS. Under ~8 dB the mastering chain has flattened the
                 performance; 10-13 dB is a mix that still breathes.
  section arc    RMS per section. A build louder than its drop means the drop
                 never lands, however good the sounds are.
  loudness       Integrated LUFS and the loudest 3 s (BS.1770-4: K-weighting,
                 400 ms blocks, gates at -70 LUFS and -10 LU). Gain-match the
                 reference on this before judging anything: louder always
                 sounds better.
  correlation    Mono compatibility. Below 0 the sides cancel on a phone.
  mono loss      Per band, what folding to mono costs: 0 dB in phase, -3 dB
                 uncorrelated, worse = cancelling. A band losing more than the
                 rest is a width trick or a phase problem living there.
  low-end mono   Side vs mid under 120 Hz — wide bass smears on club systems.

Loudness and the per-band mono check need numpy + scipy; without them those
lines say so and the rest still runs.
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
    (16000, 20000, "top"),
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


def lufs(left, right, sr):
    """(integrated, max short-term) per ITU-R BS.1770-4 / EBU R128."""
    import numpy as np
    from scipy.signal import lfilter

    x = np.array([left, right], dtype=np.float64)
    # K-weighting for any sample rate: high shelf (+4 dB above ~1.7 kHz), then
    # the RLB high-pass at ~38 Hz. Constants as in the standard's 48 kHz design.
    k = math.tan(math.pi * 1681.974450955533 / sr)
    vh, q = 10 ** (3.999843853973347 / 20), 0.7071752369554196
    vb = vh ** 0.4996667741545416
    a0 = 1 + k / q + k * k
    x = lfilter([(vh + vb * k / q + k * k) / a0, 2 * (k * k - vh) / a0, (vh - vb * k / q + k * k) / a0],
                [1, 2 * (k * k - 1) / a0, (1 - k / q + k * k) / a0], x, axis=1)
    k, q = math.tan(math.pi * 38.13547087602444 / sr), 0.5003270373238773
    a0 = 1 + k / q + k * k
    x = lfilter([1, -2, 1], [1, 2 * (k * k - 1) / a0, (1 - k / q + k * k) / a0], x, axis=1)
    sq = (x * x).sum(axis=0)  # channels summed, weights 1.0 for L and R
    cum = np.concatenate([[0.0], np.cumsum(sq)])

    def blocks(seconds, step):
        w, h = int(seconds * sr), int(step * sr)
        starts = np.arange(0, max(1, len(sq) - w + 1), h)
        return (cum[starts + w] - cum[starts]) / w if len(sq) >= w else np.array([sq.mean()])

    def loud(ms):
        return -0.691 + 10 * np.log10(ms + 1e-20)

    gate = blocks(0.4, 0.1)
    gate = gate[loud(gate) > -70]
    if not len(gate):
        return float("-inf"), float("-inf")
    rel = loud(gate.mean()) - 10
    integrated = loud(gate[loud(gate) > rel].mean())
    return float(integrated), float(loud(blocks(3.0, 1.0)).max())


def mono_loss(left, right, sr, n_fft=4096):
    """Per band: dB lost folding to mono, and side/mid under 120 Hz."""
    import numpy as np

    l, r = np.asarray(left), np.asarray(right)
    n = (min(len(l), len(r)) // n_fft) * n_fft
    if n == 0:
        return {}, None  # shorter than one FFT frame
    win = np.hanning(n_fft)
    fl = np.fft.rfft(l[:n].reshape(-1, n_fft) * win, axis=1)
    fr = np.fft.rfft(r[:n].reshape(-1, n_fft) * win, axis=1)
    el, er = (abs(fl) ** 2).sum(0), (abs(fr) ** 2).sum(0)
    em, es = (abs((fl + fr) / 2) ** 2).sum(0), (abs((fl - fr) / 2) ** 2).sum(0)
    hz = np.fft.rfftfreq(n_fft, 1 / sr)
    out = {}
    for lo, hi, name in BANDS:
        m = (hz >= lo) & (hz < hi)
        stereo = (el[m].sum() + er[m].sum()) / 2
        if stereo > 1e-12:
            # a mono fold is (L+R)/2: in phase keeps it all, uncorrelated halves it
            out[name] = 10 * math.log10(em[m].sum() / stereo + 1e-20)
    low = hz < 120
    if em[low].sum() + es[low].sum() < 1e-12:
        return out, None  # no low end to be wide or narrow
    width = 10 * math.log10((es[low].sum() + 1e-20) / (em[low].sum() + 1e-20))
    return out, width


def db(x):
    return 20 * math.log10(abs(x) + 1e-12)


def rms(seg):
    return (sum(v * v for v in seg) / max(1, len(seg))) ** 0.5


def load(path, start=None, end=None):
    """Decode and cut to [start, end) seconds — compare chorus with chorus."""
    l, r, sr = decode(path)
    i0 = int((start or 0) * sr)
    i1 = int(end * sr) if end is not None else len(l)
    if i1 <= i0 or i0 >= len(l):
        raise SystemExit(f"{path}: empty section {start}..{end} s (file is {len(l) / sr:.1f} s)")
    return l[i0:i1], r[i0:i1], sr


def profile(path, start=None, end=None):
    """Every number the report prints, as a dict (`--json`): one per
    reference section, so "my mix against the set" needs no re-decoding."""
    l, r, sr = load(path, start, end)
    n = min(len(l), len(r))
    l, r = l[:n], r[:n]
    mono = [(l[i] + r[i]) * 0.5 for i in range(n)]
    peak = max(max(abs(v) for v in l), max(abs(v) for v in r))
    num = sum(l[i] * r[i] for i in range(n))
    den = math.sqrt(sum(v * v for v in l) * sum(v * v for v in r)) or 1.0
    out = {
        "file": str(path), "start": start, "end": end, "seconds": n / sr, "sr": sr,
        "peak_db": db(peak), "rms_db": db(rms(mono)), "crest_db": db(peak) - db(rms(mono)),
        "correlation": num / den, "bands_db": band_energy(mono, sr),
    }
    try:
        out["lufs_integrated"], out["lufs_short_max"] = lufs(l, r, sr)
        out["mono_loss_db"], out["side_mid_low_db"] = mono_loss(l, r, sr)
    except ImportError:
        out["note"] = "LUFS and mono loss need numpy + scipy"
    return out


def centred(bands):
    """A band profile with its mean removed: level-independent, so a mix at
    −14 LUFS and a reference at −8 compare by shape alone."""
    m = sum(bands[name] for _, _, name in BANDS) / len(BANDS)
    return {name: bands[name] - m for _, _, name in BANDS}


def target_corridor(profiles, floor_db=1.0):
    """The genre's tonal balance from several reference profiles (`--json`
    files, one per chorus): per band the mean of the centred profiles and a
    spread (the standard deviation, never under `floor_db` so one reference
    does not pretend to be a law). Ozone's Tonal Balance does this with a
    bigger library; the numbers are the same kind."""
    if not profiles:
        raise ValueError("no reference profiles")
    cs = [centred(p["bands_db"]) for p in profiles]
    out = {}
    for _, _, name in BANDS:
        xs = [c[name] for c in cs]
        mean = sum(xs) / len(xs)
        var = sum((x - mean) ** 2 for x in xs) / len(xs)
        out[name] = (mean, max(math.sqrt(var), floor_db))
    return out


def target_verdict(bands, corridor):
    """Per band: how far the mix's centred profile sits outside the corridor
    (0 inside; positive = above = too much of that band)."""
    c = centred(bands)
    out = {}
    for _, _, name in BANDS:
        mean, spread = corridor[name]
        d = c[name] - mean
        out[name] = 0.0 if abs(d) <= spread else (d - spread if d > 0 else d + spread)
    return out


def load_profiles(paths):
    """`--target` arguments: profile JSON files, or directories of them."""
    import json
    files = []
    for p in paths:
        p = Path(p)
        files += sorted(p.glob("*.json")) if p.is_dir() else [p]
    profiles = []
    for f in files:
        d = json.loads(f.read_text())
        if "bands_db" not in d:
            raise ValueError(f"{f}: not a mixcheck profile (no bands_db)")
        profiles.append(d)
    return profiles


def print_target(mine, corridor, n_refs):
    verdict = target_verdict(mine, corridor)
    c = centred(mine)
    print(f"\n   target corridor from {n_refs} reference profile(s), level-independent (each profile mean-centred)")
    print("   band            this mix    target       verdict")
    for _, _, name in BANDS:
        mean, spread = corridor[name]
        v = verdict[name]
        tag = "inside" if v == 0.0 else (f"above by {v:.1f} dB  ←" if v > 0 else f"below by {-v:.1f} dB  ←")
        print(f"   {name:9s} {c[name]:+6.1f} dB   {mean:+6.1f} ±{spread:3.1f}   {tag}")
    print("   (move EQ / Multiband only where a band is outside; inside the corridor is taste)")


def report(path, sections=None, ref=None, span=(None, None), ref_span=(None, None), target=None):
    l, r, sr = load(path, *span)
    n = min(len(l), len(r))
    mono = [(l[i] + r[i]) * 0.5 for i in range(n)]

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

    try:
        integrated, short_max = lufs(l[:n], r[:n], sr)
        print(f"   loudness {integrated:6.1f} LUFS integrated    {short_max:6.1f} LUFS loudest 3 s")
    except ImportError:
        integrated = None
        print("   loudness: needs numpy + scipy")

    # Mono compatibility: correlation between channels, per-band fold loss, and
    # how wide the low end is.
    num = sum(l[i] * r[i] for i in range(n))
    den = math.sqrt(sum(v * v for v in l) * sum(v * v for v in r)) or 1.0
    corr = num / den
    print(f"   correlation {corr:+.2f}" + ("   ← sides cancel in mono" if corr < 0.2 else ""))
    try:
        loss, width = mono_loss(l[:n], r[:n], sr)
        worst = min(loss.values()) if loss else 0.0
        print("   mono loss  " + "  ".join(f"{k} {v:+.1f}" for k, v in loss.items()) + " dB"
              + ("   ← a band cancels in mono" if worst < -4.5 else ""))
        if width is None:
            print("   side/mid under 120 Hz: no low end")
        else:
            print(f"   side/mid under 120 Hz {width:+.1f} dB" + ("   ← wide bass" if width > -12 else ""))
    except ImportError:
        print("   mono loss: needs numpy")

    mine = band_energy(mono, sr)
    theirs = None
    if ref:
        rl, rr, rsr = load(ref, *ref_span)
        rn = min(len(rl), len(rr))
        theirs = band_energy([(rl[i] + rr[i]) * 0.5 for i in range(rn)], rsr)
        try:
            ri, _ = lufs(rl[:rn], rr[:rn], rsr)
            if integrated is not None:
                d = integrated - ri
                print(f"   reference {ri:6.1f} LUFS — this mix is {d:+.1f} LU"
                      + ("   ← gain-match before comparing by ear" if abs(d) > 1 else ""))
        except ImportError:
            pass

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
    if target:
        print_target(mine, target_corridor(target), len(target))

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
    ap.add_argument("--start", type=float, help="seconds: measure from here (same section as the reference)")
    ap.add_argument("--end", type=float, help="seconds: measure up to here")
    ap.add_argument("--ref-start", type=float, help="the reference's section start, seconds")
    ap.add_argument("--ref-end", type=float, help="the reference's section end, seconds")
    ap.add_argument("--json", action="store_true", help="print the mix's profile as JSON instead of the report")
    ap.add_argument("--target", nargs="+", metavar="PROFILE", help="reference profiles (--json files or a directory of them): the genre's tonal corridor, mean ± spread per band, level-independent; the report says where this mix leaves it")
    a = ap.parse_args()
    if a.json:
        import json

        def finite(v):
            # JSON has no NaN/inf (silence reads -inf LUFS): null instead.
            if isinstance(v, float):
                return v if math.isfinite(v) else None
            if isinstance(v, dict):
                return {k: finite(x) for k, x in v.items()}
            if isinstance(v, (list, tuple)):
                return [finite(x) for x in v]
            return v
        p = profile(a.mix, a.start, a.end)
        if a.target:
            refs = load_profiles(a.target)
            corridor = target_corridor(refs)
            p["target"] = {
                "profiles": len(refs),
                "corridor_db": {k: {"mean": m, "spread": s} for k, (m, s) in corridor.items()},
                "outside_db": target_verdict(p["bands_db"], corridor),
            }
        print(json.dumps(finite(p), indent=1))
        return

    sections = None
    if a.bpm and a.bars:
        edges = [0] + [int(x) for x in a.bars.split(",")]
        sections = (a.bpm, [(edges[i], edges[i + 1], f"bars {edges[i]}-{edges[i+1]}")
                            for i in range(len(edges) - 1)])
    report(a.mix, sections, a.ref, (a.start, a.end), (a.ref_start, a.ref_end),
           load_profiles(a.target) if a.target else None)


if __name__ == "__main__":
    main()
