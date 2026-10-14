"""Score → stems → mix, with fixed gains. Nothing here measures or corrects.

Gain staging is one stage: every stem (sdsp-chain instruments and the
python-rendered drums alike) is written at 0 dB, and its level is the
`gain_db` / `gain_automate` of its track in the mix toml — the role gain from
instruments.toml plus the preset trim plus the theme's style gains, plus
whatever `fit` added. The whole spec is saved as `mix.json` beside the audio,
so a mix is reproducible and `check` can read the gains it measured under.
"""

import json
import re
import shutil
import subprocess
from dataclasses import asdict, dataclass, field
from pathlib import Path

from . import config
from . import drums as td
from .music import Score, role_of, track_of

PAUSE_DEPTH = 6.0  # dB the music sits lower under speech than in a pause


@dataclass
class MixSpec:
    gains: dict[str, float]
    depth: float = PAUSE_DEPTH
    zones: list = field(default_factory=list)  # [(start, end, kind, dB)]
    shelf: dict[str, float] = field(default_factory=dict)
    limiter_in: float = 0.0

    def save(self, path) -> None:
        Path(path).write_text(json.dumps(asdict(self), ensure_ascii=False, indent=1) + "\n")

    @classmethod
    def load(cls, path) -> "MixSpec":
        return cls(**json.loads(Path(path).read_text()))


def _params(d: dict) -> str:
    return "{ " + ", ".join(f'"{k}" = {float(v)}' for k, v in d.items()) + " }"


def _stages(stages) -> list[str]:
    out = []
    for st in stages:
        out += ["", "  [[track.stage]]", f'  plugin = "{st["plugin"]}"', f"  params = {_params(st['params'])}"]
    return out


def stem_toml(track: str, notes: list, length: float) -> str:
    m = re.match(r"^(\w+)-([a-z]+)(\d+)$", track)
    if not m:
        raise ValueError(f"имя партии {track}: нужно <роль>-<инструмент><пресет>")
    role, inst, preset = m.group(1), m.group(2), int(m.group(3))
    stages = config.cfg()["stages"]
    lines = [
        f"# {track} — sdsp-score",
        f'out = "stem_{track}.wav"',
        f"sample_rate = {config.cfg()['sample_rate']}",
        "tail_s = 0.0",
        "",
        "[[track]]",
        f'name = "{track}"',
        f'instrument = "{inst}"',
        f"preset = {preset}",
        "gain_db = 0.0",
        f"duration_s = {length + 4.0:.3f}",
        "notes = [",
    ]
    lines += [f"  [{at:.3f}, {dur:.3f}, {p}, {v}]," for at, dur, p, v in notes]
    lines.append("]")
    lines += _stages(stages.get("lead" if role in ("thread", "accent") else role, []))
    return "\n".join(lines) + "\n"


def gain_of(score: Score, track: str) -> float:
    cfg = config.cfg()
    role = role_of(track)
    trim = cfg["lead_trim"].get(track.split("-", 1)[1], 0.0) if role in ("lead", "thread", "accent") else 0.0
    base = cfg["role_gain"].get(role, 0.0)
    if role not in ("lead", "thread", "accent", "pad", "arp", "bass", "ostinato"):
        return base  # ударные, 808, бит — не партии стиля
    for ph in score.phrases:  # стиль темы может сдвинуть громкость роли
        if track_of(ph.theme.style, role) == track:
            return base + trim + float(ph.theme.style.get("gains", {}).get(role, 0.0))
    return base + trim


def initial_spec(score: Score, names: list[str]) -> MixSpec:
    return MixSpec({n: gain_of(score, n) for n in names}, zones=[[a, b, k, 0.0] for a, b, k in zones_of(score)])


# ---------- automation ----------


def zone_value(zones, t: float, ramp=0.5) -> float:
    """Громкость зоны в момент t; на границе — линейный переход за `ramp`."""
    for i, (a, b, db) in enumerate(zones):
        if a <= t < b:
            if i + 1 < len(zones) and t > b - ramp / 2:
                nxt = zones[i + 1][2]
                return db + (nxt - db) * (t - (b - ramp / 2)) / ramp
            if i > 0 and t < a + ramp / 2:
                prev = zones[i - 1][2]
                return prev + (db - prev) * (t - (a - ramp / 2)) / ramp
            return db
    return 0.0


def automation(score: Score, track: str, depth: float, ramp=0.3, zones=()) -> list[tuple[float, float]]:
    """Громкость стема во времени: под речью −depth, в паузе 0, плюс подъёмы
    ролей по фразам, минус 90 дБ в дырках, плюс громкость зоны."""
    role = role_of(track)
    talk = [] if role == "accent" else score.speech
    ups = [(a, b, db) for r, a, b, db in score.lifts if r == role]

    def env(t, a, b):
        return max(0.0, min(1.0, (t - a + ramp) / ramp, (b + ramp - t) / ramp))

    holes = [(a + ramp, b - ramp) for a, b in score.holes]  # дырка — тишина и в хвостах
    ts = sorted(
        {0.0, score.length + 4.0}
        | {max(0.0, x) for a, b in [*talk, *[(a, b) for a, b, _ in ups], *holes] for x in (a - ramp, a, b, b + ramp)}
        | {max(0.0, x) for a, _b, _ in list(zones)[1:] for x in (a - 0.25, a + 0.25)}
    )
    pts = [
        (
            t,
            zone_value(zones, t)
            - depth * max([env(t, a, b) for a, b in talk] + [0.0])
            + sum(db * env(t, a, b) for a, b, db in ups)
            - 90.0 * max([env(t, a, b) for a, b in holes] + [0.0]),
        )
        for t in ts
    ]
    return [
        p
        for i, p in enumerate(pts)
        if i in (0, len(pts) - 1) or abs(p[1] - pts[i - 1][1]) > 1e-6 or abs(p[1] - pts[i + 1][1]) > 1e-6
    ]


def _overlay(zs, a, b, kind):
    cut = []
    for za, zb, k in zs:
        if zb <= a or za >= b:
            cut.append((za, zb, k))
            continue
        if za < a:
            cut.append((za, a, k))
        if zb > b:
            cut.append((b, zb, k))
    return sorted(cut + [(a, b, kind)])


def zones_of(score: Score) -> list[tuple[float, float, str]]:
    """Зоны громкости — по фразам. Первые два такта модуляции и проводка-акцент
    поверх речи — как карточка: вровень с живым звуком."""
    zs = [(p.start, p.end, p.section.kind) for p in score.phrases]
    for ph in score.phrases:
        if ph.note.startswith("модуляция"):
            zs = _overlay(zs, ph.start, min(ph.end, ph.start + 2 * ph.bar), "card")
    acc = sorted((at, at + d) for t, ns in score.notes.items() if role_of(t) == "accent" for at, d, _, _ in ns)
    runs: list[list[float]] = []
    for a, b in acc:
        if runs and a - runs[-1][1] < 1.0:
            runs[-1][1] = max(runs[-1][1], b)
        else:
            runs.append([a, b])
    for a, b in runs:
        zs = _overlay(zs, a, b, "card")
    for a, b in score.holes:  # дырка — своя зона, её не меряем
        zs = _overlay(zs, max(0.0, a), min(score.length, b), "hole")
    return [z for z in zs if z[1] - z[0] > 0.05]


def mix_toml(score: Score, spec: MixSpec, out: str, key_audio: bool, dry=False) -> str:
    lines = ["# микс стемов — sdsp-score", f'out = "{out}"', "tail_s = 0.0"]
    if key_audio and not dry:
        lines.append('sidechain = "key.wav"')
    zones = [(a, b, db) for a, b, _k, db in spec.zones]
    for n, g in spec.gains.items():
        lines += ["", "[[track]]", f'name = "{n}"', f'input = "stem_{n}.wav"']
        pts = automation(score, n, 0.0 if dry else spec.depth, zones=zones)
        if len(pts) > 2 or any(abs(v) > 1e-6 for _, v in pts):
            # gain_automate в sdsp-chain ЗАМЕНЯЕТ gain_db: громкость стема в каждой точке
            lines.append("gain_automate = [" + ", ".join(f"[{t:.2f}, {v + g:.2f}]" for t, v in pts) + "]")
        else:
            lines.append(f"gain_db = {g:.2f}")
        if abs(spec.shelf.get(n, 0.0)) > 1e-6:
            lines += [
                "  [[track.stage]]",
                '  plugin = "eq"',
                f'  params = {{ "Low Freq" = 150.0, "Low Gain" = {spec.shelf[n]:.1f} }}',
            ]
    for st in config.cfg()["master"]:
        params = dict(st["params"])
        if st["plugin"] == "limiter":
            params["Input"] = params.get("Input", 0.0) + spec.limiter_in
        lines += ["", "[[master]]", f'plugin = "{st["plugin"]}"', f"params = {_params(params)}"]
    return "\n".join(lines) + "\n"


# ---------- render ----------


class RenderError(RuntimeError):
    pass


def chain(out: Path, cfg: str) -> None:
    r = subprocess.run([config.chain_bin(), cfg], check=False, cwd=out, capture_output=True, text=True)
    if r.returncode:
        raise RenderError(f"sdsp-chain {cfg}: {r.stderr[-400:]}")


def write_stem_tomls(out: Path, score: Score) -> list[str]:
    names = [t for t, ns in score.notes.items() if ns]
    for t in names:
        f, text = out / f"stem_{t}.toml", stem_toml(t, score.notes[t], score.length)
        if not f.exists() or f.read_text() != text:  # тот же toml — тот же стем, его не трогаем
            f.write_text(text)
    return names


def render_stems(out: Path, score: Score, hits, subs, bed=None, bsecs=()) -> list[str]:
    names = write_stem_tomls(out, score)
    for t in names:
        wav, toml = out / f"stem_{t}.wav", out / f"stem_{t}.toml"
        if wav.exists() and wav.stat().st_mtime_ns > toml.stat().st_mtime_ns:
            print(f"стем {t}: тот же toml — беру готовый")
            continue
        chain(out, f"stem_{t}.toml")
        print(f"стем {t}: {len(score.notes[t])} нот")
    if hits:
        td.render_drums(out, hits, score.length)
        names.append("drums")
    if subs:
        td.render_sub(out, subs, score.length)
        names.append("sub808")
    if bed and bsecs:
        td.render_bed(out, Path(bed["path"]), bsecs, bed["bpm"], score.length)
        names.append("bed")
    return names


def render_mix(out: Path, score: Score, spec: MixSpec, key_audio: bool) -> None:
    spec.save(out / "mix.json")
    (out / "score.toml").write_text(mix_toml(score, spec, "score.wav", key_audio))
    chain(out, "score.toml")
    if not key_audio and not score.speech:  # сухой микс совпал бы байт в байт
        shutil.copyfile(out / "score.wav", out / "score-dry.wav")
        return
    (out / "score-dry.toml").write_text(mix_toml(score, spec, "score-dry.wav", key_audio, dry=True))
    chain(out, "score-dry.toml")
