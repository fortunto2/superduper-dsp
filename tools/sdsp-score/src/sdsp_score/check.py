"""Read a rendered score and say how it sits. Reads only — never renders, never writes audio.

One unit everywhere: **short-term LUFS** (EBU R128, 3 s window, sampled every
0.1 s by ffmpeg's ebur128), power-averaged over a window. A stem's level in
the mix is its own short-term LUFS plus its static `gain_db` from mix.json
(zone and speech automation are not added: they move every part together,
so they cancel in the part-against-part readings). Relative readings are dB
between two such numbers; against the film sound it is music minus `key.wav`.

    sdsp-score check <out>           # prints, writes check.json

Readings: integrated LUFS; LUFS per section; each lead over the loudest
accompaniment (pad / arp / ostinato) per motif phrase; low (808 + bass),
drums and bed against the lead; low band against mid band of the mix; music
against the key per zone; stretches under −60 LUFS longer than 20 s.
"""

import json
import math
import re
import subprocess
from functools import cache
from pathlib import Path

from . import config

LEAD_TEXTURES = ("motif", "motif_seq", "motif_inv", "motif_retro", "motif_aug", "card")
SILENT = -70.0
FLOOR = -60.0
GROUP_NAMES = {"low": "808+бас", "drums": "ударные", "bed": "бит"}


def st_curve(wav: str, af: str = "") -> tuple[tuple[tuple[float, float], ...], float]:
    """((t, S), …) every 0.1 s and the integrated LUFS; cached while the file is unchanged."""
    st = Path(wav).stat()
    return _st_curve(wav, af, st.st_mtime_ns, st.st_size)


@cache
def _st_curve(wav: str, af: str, _mtime: int, _size: int) -> tuple[tuple[tuple[float, float], ...], float]:
    chainf = (af + "," if af else "") + "ebur128"
    r = subprocess.run(
        [config.ffmpeg(), "-nostats", "-v", "verbose", "-i", wav, "-af", chainf, "-f", "null", "-"],
        capture_output=True,
        text=True,
        check=False,
    )
    if r.returncode:
        raise RuntimeError(f"ffmpeg ebur128 {wav}: {r.stderr[-300:]}")
    pts = tuple(
        (float(t), max(SILENT * 2, float(s)))
        for t, s in re.findall(r" t: ([\d.]+)\s+TARGET.*?S:\s*(-?[\d.]+|-inf)", r.stderr.replace("-inf", "-140"))
    )
    m = re.findall(r"I:\s+(-?[\d.]+) LUFS", r.stderr)
    integ = float(m[-1]) if m else SILENT
    return pts, (integ if integ > SILENT else SILENT)


def power_mean(vals) -> float:
    vals = list(vals)
    if not vals:
        return SILENT * 2
    p = sum(10 ** (v / 10) for v in vals) / len(vals)
    return 10 * math.log10(p) if p > 0 else SILENT * 2


def window(curve, a: float, b: float) -> float | None:
    """Short-term LUFS over [a, b): the samples whose 3 s window lies mostly inside."""
    lo = a + min(3.0, (b - a) / 2)
    xs = [s for t, s in curve if lo <= t <= b]
    return power_mean(xs) if xs else None


def psum(dbs) -> float:
    return 10 * math.log10(sum(10 ** (d / 10) for d in dbs) or 1e-14)


def _median(xs):
    xs = sorted(xs)
    return xs[len(xs) // 2] if xs else None


def check(out) -> dict:
    out = Path(out)
    sc = json.loads((out / "score.json").read_text())
    mix = json.loads((out / "mix.json").read_text())
    gains = mix["gains"]
    res: dict = {"unit": "short-term LUFS (3 s), power mean"}
    curve, res["integrated"] = st_curve(str(out / "score.wav"))
    res["sections"] = [
        {
            "name": s["name"],
            "kind": s["kind"],
            "start": s["start"],
            "end": s["end"],
            "lufs": window(curve, s["start"], s["end"]),
        }
        for s in sc["sections"]
    ]

    stems = {n: st_curve(str(out / f"stem_{n}.wav"))[0] for n in gains if (out / f"stem_{n}.wav").exists()}

    def level(names, a, b):
        vals = [window(stems[n], a, b) for n in names if n in stems]
        vals = [v + gains[n] for v, n in zip(vals, [n for n in names if n in stems]) if v is not None]
        return psum(vals) if vals else None

    by_role: dict[str, list[str]] = {}
    for n in stems:
        by_role.setdefault(n.split("-")[0], []).append(n)
    motif = [p for p in sc["phrases"] if p["texture"] in LEAD_TEXTURES and p["lead"] in stems]
    lead_acc: dict[str, list[float]] = {}
    for p in motif:
        lead = level([p["lead"]], p["start"], p["end"])
        accs = [level(by_role.get(r, []), p["start"], p["end"]) for r in ("pad", "arp", "ostinato")]
        accs = [x for x in accs if x is not None and x > SILENT]
        if lead is not None and accs:
            lead_acc.setdefault(p["lead"], []).append(round(lead - max(accs), 2))
    res["lead_vs_acc"] = {k: {"median": _median(v), "worst": min(v), "phrases": len(v)} for k, v in lead_acc.items()}

    view = [p for p in motif if p["texture"] != "card"]
    leads = by_role.get("lead", [])
    res["vs_lead"] = {}
    for label, names in (("low", ["sub808", *by_role.get("bass", [])]), ("drums", ["drums"]), ("bed", ["bed"])):
        diffs = []
        for p in view:
            g, ld = level(names, p["start"], p["end"]), level(leads, p["start"], p["end"])
            if g is not None and ld is not None and g > SILENT and ld > SILENT:
                diffs.append(g - ld)
        if diffs:
            res["vs_lead"][label] = round(_median(diffs), 2)

    low = st_curve(str(out / "score.wav"), "lowpass=f=150:poles=2")[1]
    mid = st_curve(str(out / "score.wav"), "highpass=f=150:poles=2,lowpass=f=4000")[1]
    res["low_vs_mid"] = round(low - mid, 2)

    key = out / "key.wav"
    if key.exists():
        kc = st_curve(str(key))[0]
        zones = []
        for a, b, kind, _db in mix["zones"]:
            if kind == "hole" or b - a < 4.0:
                continue
            m, k = window(curve, a, b), window(kc, a, b)
            if m is not None and k is not None and k > FLOOR:
                zones.append({"start": a, "end": b, "kind": kind, "music_vs_key": round(m - k, 2)})
        res["key_zones"] = zones

    dry, _ = st_curve(str(out / "score-dry.wav"))
    holes = sc.get("holes", [])
    gaps, run = [], None
    for t, s in [*dry, (math.inf, 0.0)]:
        quiet = s < FLOOR and not any(lo <= t < hi for lo, hi in holes)
        if quiet and run is None:
            run = t
        elif not quiet and run is not None:
            if min(t, sc["length"]) - run > 20.0:
                gaps.append([run, min(t, sc["length"])])
            run = None
    res["gaps"] = gaps
    (out / "check.json").write_text(json.dumps(res, ensure_ascii=False, indent=1) + "\n")
    return res


def lines(res: dict) -> list[str]:
    out = [f"микс целиком {res['integrated']:.1f} LUFS-I; единица — {res['unit']}"]
    for lead, d in res["lead_vs_acc"].items():
        out.append(
            f"{lead} над сопровождением в {d['phrases']} мотивных фразах: медиана {d['median']:+.1f}, "
            f"худшая {d['worst']:+.1f} дБ"
        )
    for k, v in res["vs_lead"].items():
        out.append(f"{GROUP_NAMES[k]} к лиду {v:+.1f} дБ")
    out.append(f"низ (<150 Гц) к середине {res['low_vs_mid']:+.1f} дБ")
    by: dict[str, list[float]] = {}
    for z in res.get("key_zones", []):
        by.setdefault(z["kind"], []).append(z["music_vs_key"])
    if by:
        out.append(
            "музыка к ключу по зонам (медиана): "
            + "; ".join(f"{k} {_median(v):+.1f} ({len(v)} шт.)" for k, v in by.items())
        )
    out.append(
        "пустот длиннее 20 с нет"
        if not res["gaps"]
        else "ПУСТОТЫ: " + ", ".join(f"{a:.1f}–{b:.1f}" for a, b in res["gaps"])
    )
    return out
