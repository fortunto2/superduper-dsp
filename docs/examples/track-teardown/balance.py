#!/usr/bin/env python3
"""Solve the track gains instead of guessing them.

Band balance is relative: every band is measured against the loudest one. So
raising the bass to fill 120-250 Hz pushes all six other bands down, the next
edit fixes one of those and breaks two more, and the loop does not converge.
Four hand passes on this mix went -11.7 → -13.8 → -10.1 → -9.8 dB in the band
they were aimed at.

The fix is to stop editing and start solving. Render each track on its own,
measure how much energy it puts in each band, and fit the gains whose sum
lands closest to the reference profile. One render per track, one solve, done.

    .venv/bin/python balance.py            # solve and write GAINS back
    .venv/bin/python balance.py --dry      # solve and print, change nothing
"""

import argparse
import re
import subprocess
import sys
from pathlib import Path

import numpy as np
from scipy.optimize import minimize

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import build  # noqa: E402

BANDS = [(20, 60, "sub"), (60, 120, "kick"), (120, 250, "low"),
         (250, 800, "body"), (800, 2500, "mid"), (2500, 6000, "presence"),
         (6000, 16000, "air")]

# Measured from the reference with mixcheck.py.
TARGET = np.array([-3.1, -0.6, 0.0, -6.5, -0.2, -9.9, -12.2])

SOLO = HERE / "solo"
SOLO.mkdir(exist_ok=True)

# Spectrum is not the only thing a layer is for. Left free, the solver put the
# bass at -28.6 dB: it occupies the same bands as the kick, so dropping it fits
# the curve slightly better. But the kick is a noise transient and the bass is
# what carries the pitch — a track with no audible bass note measures fine and
# has no key. These floors say "this layer must remain audible", and the solver
# balances everything else around them.
FLOORS = {
    "bass": (-12.0, 6.0),
    "kick": (-18.0, 6.0),
    # The top needs the most headroom of anything here: it starts 30 dB down
    # in the intro and the master bus eats several more dB of it, so a +12
    # ceiling had the solver pinned against the bound with 8 dB still to find.
    "air": (-30.0, 26.0),
}


def band_energy(path):
    """Mean energy per band, linear — so gains add as squares, not in dB."""
    import soundfile as sf
    y, sr = sf.read(path, always_2d=True)
    y = y.mean(axis=1)
    n = 1 << 15
    hop = n // 2
    win = np.hanning(n)
    freqs = np.fft.rfftfreq(n, 1 / sr)
    acc = np.zeros(len(BANDS))
    frames = 0
    for i in range(0, len(y) - n, hop * 4):
        spec = np.abs(np.fft.rfft(y[i:i + n] * win)) ** 2
        for b, (lo, hi, _) in enumerate(BANDS):
            sel = (freqs >= lo) & (freqs < hi)
            # SUM, not mean — this is what mixcheck.py does, and the two must
            # agree or the solver optimises a different quantity than the one
            # being reported. Averaging per bin hands narrow bands (sub is
            # 40 Hz wide) the same weight as air's 10 kHz, and the solved mix
            # measured 13 dB off in the very bands the solver called perfect.
            acc[b] += spec[sel].sum() if sel.any() else 0.0
        frames += 1
    return acc / max(frames, 1)


def render_solo(name):
    """One track alone, through its own stages and the master EQ.

    The master EQ is included because it shapes every band by a fixed amount;
    the compressor and limiter are not, because they are non-linear and would
    make the per-track energies stop adding up.
    """
    out = SOLO / f"{name}.wav"
    cfg = build.mix_config(out, solo=name, master="eq")
    build.run_chain(cfg, f"solo_{name}")
    return out


def solve(energies, names, target=TARGET):
    """Gains (dB) whose summed spectrum best matches the reference shape."""
    E = np.array([energies[n] for n in names])          # (tracks, bands)
    start = np.array([build.GAINS.get(n, 0.0) for n in names])

    def loss(g):
        a = 10 ** (g / 10.0)                            # dB → energy factor
        total = (a[:, None] * E).sum(axis=0)
        db = 10 * np.log10(total + 1e-30)
        db -= db.max()                                  # balance is relative
        return float(((db - target) ** 2).sum())

    # Bounds matter: unconstrained, the solver "fixed" the missing 120-250 Hz
    # band by muting four layers at -54000 dB, which fits the shape and is not
    # a mix. A layer may be pulled 30 dB down or pushed 12 up, no further — if
    # the target still cannot be met inside that, the answer is a missing
    # sound source, not a fader.
    bounds = [FLOORS.get(n, (-30.0, 12.0)) for n in names]
    res = minimize(loss, start, method="L-BFGS-B", bounds=bounds)
    return res.x, loss(start), res.fun


def measure_mix(path):
    """Band balance of a finished file, normalised the way mixcheck does."""
    e = band_energy(path)
    db = 10 * np.log10(e + 1e-30)
    return db - db.max()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dry", action="store_true")
    ap.add_argument("--iterate", type=int, default=1,
                    help="close the loop around the master bus this many times")
    a = ap.parse_args()

    names = [n for n, _, _ in build.track_defs()] + ["air"]
    energies = {}
    for n in names:
        print(f"  rendering {n} solo…", flush=True)
        energies[n] = band_energy(render_solo(n))

    print("\nper-track band energy (dB, each track's own peak = 0):")
    print(f"  {'track':8s} " + " ".join(f"{lbl:>9s}" for _, _, lbl in BANDS))
    for n in names:
        db = 10 * np.log10(energies[n] + 1e-30)
        db -= db.max()
        print(f"  {n:8s} " + " ".join(f"{v:9.1f}" for v in db))

    # The solve is linear; the master bus is not. A compressor and a limiter
    # squash the loudest band hardest, and the low end has the biggest peaks —
    # the first closed-form answer predicted 0.0 dB at 120-250 and the render
    # came back 12 dB under. So aim off: measure the finished mix, and move
    # the target by whatever the master bus took away.
    target = TARGET.copy()
    best = None
    for it in range(max(1, a.iterate)):
        g, before, after = solve(energies, names, target)
        print(f"\npass {it+1}: fit error {before:.1f} → {after:.1f}")
        for n, v in zip(names, g):
            old = build.GAINS.get(n, build.AIR_TRIM if n == "air" else 0.0)
            print(f"  {n:8s} {old:+6.1f} → {v:+6.1f} dB   ({v-old:+.1f})")

        if a.dry:
            break
        write_gains(names, g)

        out = build.mix()
        got = measure_mix(out)
        err = got - TARGET
        score = float(np.sqrt((err ** 2).mean()))
        print(f"\n  {'band':9s} {'rendered':>9s} {'wanted':>9s} {'error':>7s}")
        for (lo, hi, lbl), r_, w_, e_ in zip(BANDS, got, TARGET, err):
            print(f"  {lbl:9s} {r_:9.1f} {w_:9.1f} {e_:+7.1f}")
        # Keep the best MEASURED pass, not the last one. The loop ended on
        # whatever it happened to try last, wrote those gains, and left the
        # low end 13 dB down — a worse mix than pass 2 had already found.
        if best is None or score < best[0]:
            best = (score, g.copy())
            print(f"  rms error {score:.2f} dB — best so far")
        else:
            print(f"  rms error {score:.2f} dB (best {best[0]:.2f})")
        if it + 1 >= max(1, a.iterate):
            break
        # Damped, and clamped. A full-step correction diverged: 10.7 → 10.5 →
        # 14.0 dB worst error, because moving the target by the whole error
        # asks the solver for a shape the layers cannot make, and it answers
        # with extreme gains whose non-linear behaviour is worse still.
        target = np.clip(target - 0.45 * err, TARGET - 12.0, TARGET + 12.0)
        print(f"  worst error {np.abs(err).max():.1f} dB — compensating")

    if best is not None:
        print(f"\nwriting best pass (rms {best[0]:.2f} dB)")
        write_gains(names, best[1])
        build.mix()


def write_gains(names, g):
    src = (HERE / "build.py").read_text()
    for n, v in zip(names, g):
        if n == "air":
            src = re.sub(r"AIR_TRIM = [-\d.]+", f"AIR_TRIM = {v:.1f}", src)
        else:
            src = re.sub(rf'("{n}": )[-\d.]+', rf"\g<1>{v:.1f}", src, count=1)
    (HERE / "build.py").write_text(src)
    import importlib
    importlib.reload(build)


if __name__ == "__main__":
    main()
