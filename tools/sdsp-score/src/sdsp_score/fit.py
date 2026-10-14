"""--fit: move mix gains until `check` reads inside the targets. Optional, on top of mix + check.

Each pass is one mix render plus one check; stems are never re-rendered,
because their level lives only in the mix gains. All targets are in the
check's unit (short-term LUFS differences):

- every lead: median 3..9 dB over the loudest accompaniment, worst phrase >= 0;
- 808+bass −6, drums −3, bed −1 dB against the lead in motif phrases;
- low band against mid band of the mix −7..−5 dB;
- with a key (film sound): music per zone at ZONE_TARGET dB from the key;
- last, the whole mix to its loudness (song −14 LUFS-I, film underlay −20) via the limiter input.
"""

from pathlib import Path

from . import check as ck
from .mix import MixSpec, render_mix

VS_LEAD = {"low": -6.0, "drums": -3.0, "bed": -1.0}
LOW_RANGE = (-7.0, -5.0)
ZONE_TARGET = {
    "view": -5.0,
    "talk": -13.5,
    "card": 0.0,
    "intro": -8.0,
    "verse": -5.0,
    "chorus": -3.0,
    "bridge": -5.0,
    "outro": -8.0,
}
LEAD_RANGE = (3.0, 9.0)


def clamp(x, lo, hi):
    return max(lo, min(hi, x))


def adjust(res: dict, spec: MixSpec, target_lufs: float | None = None) -> list[str]:
    """One correction from one reading; returns what moved (empty — nothing to do)."""
    moved = []
    for lead, d in res["lead_vs_acc"].items():
        lift = max(-d["worst"], LEAD_RANGE[0] - d["median"])
        if lift <= 0:  # не тонет; тише — только если торчит выше диапазона
            lift = -min(d["median"] - 6.0, d["worst"]) if d["median"] > LEAD_RANGE[1] else 0.0
        if abs(lift) >= 0.5:
            spec.gains[lead] += clamp(lift, -12, 12)
            moved.append(f"{lead} {lift:+.1f} дБ (медиана {d['median']:+.1f}, худшая {d['worst']:+.1f})")
    groups = {
        "low": [n for n in spec.gains if n == "sub808" or n.startswith("bass-")],
        "drums": ["drums"],
        "bed": ["bed"],
    }
    for k, target in VS_LEAD.items():
        if k in res["vs_lead"] and abs(res["vs_lead"][k] - target) >= 1.0:
            delta = clamp(target - res["vs_lead"][k], -18, 18)
            for n in groups[k]:
                if n in spec.gains:
                    spec.gains[n] += delta
            moved.append(f"{ck.GROUP_NAMES[k]} {delta:+.1f} дБ (было {res['vs_lead'][k]:+.1f}, цель {target:+.0f})")
    low = res["low_vs_mid"]
    if not LOW_RANGE[0] <= low <= LOW_RANGE[1] and not moved:
        shift = sum(LOW_RANGE) / 2 - low
        for n in groups["low"]:
            if n in spec.gains:
                spec.gains[n] = max(-40.0, spec.gains[n] + shift)
        for n in ("drums", "bed"):
            if n in spec.gains:
                spec.shelf[n] = clamp(spec.shelf.get(n, 0.0) + shift, -15.0, 0.0)
        moved.append(f"низ к середине {low:+.1f} → бас {shift:+.1f} дБ")
    for z in res.get("key_zones", []):
        err = ZONE_TARGET.get(z["kind"], -5.0) - z["music_vs_key"]
        if abs(err) >= 1.5:
            for zone in spec.zones:
                if abs(zone[0] - z["start"]) < 1e-6 and abs(zone[1] - z["end"]) < 1e-6:
                    zone[3] = clamp(zone[3] + clamp(err, -12, 12), -30.0, 24.0)
            moved.append(f"зона {z['kind']} {z['start']:.0f}с {err:+.1f}")
    if target_lufs is not None and not moved and abs(res["integrated"] - target_lufs) >= 1.0:
        step = clamp(target_lufs - res["integrated"], -12, 12)
        spec.limiter_in = clamp(spec.limiter_in + step, -12.0, 18.0)
        moved.append(f"громкость {res['integrated']:.1f} → цель {target_lufs:.0f} LUFS: лимитер {step:+.1f} дБ")
    return moved


def fit(
    out: Path, score, spec: MixSpec, key_audio: bool, passes=6, target_lufs: float | None = None
) -> tuple[dict, list[str]]:
    log: list[str] = []
    res = ck.check(out)
    for i in range(passes):
        moved = adjust(res, spec, target_lufs)
        if not moved:
            break
        log.append(f"проход {i + 1}: " + "; ".join(moved[:6]) + (" …" if len(moved) > 6 else ""))
        render_mix(out, score, spec, key_audio)
        res = ck.check(out)
    return res, log
