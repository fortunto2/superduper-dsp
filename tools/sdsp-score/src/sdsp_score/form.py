"""form.json — the one input the core reads.

A form is sections on a time line plus what the music must respect: cuts,
speech windows, holes, accents, and optionally the film sound to sit under
(`key_audio`). A film plan becomes a form in video-analyzer
(`scripts/otio_to_form.py`); a song form is generated here from
`--form "intro:8,verse:16,…"` or `--length 180`.

    {"length": 347.5,
     "sections": [{"start": 0, "end": 28.4, "kind": "intro", "name": "intro", "bars": 8, "bpm": 67.7}],
     "cuts": [], "speech": [[a, b]], "holes": [[a, b]], "accents": [t],
     "key_audio": "voice.wav", "theme2_for": ["view", "03"], "tempo_map": [[lo, hi, bpm]],
     "bed": {"path": "beat.wav", "key": "Dm", "bpm": 90}, "demo": [[a, b], …]}

Song section kinds: intro / verse / chorus / bridge / outro. Film kinds:
card / talk / view.
"""

import json
import os
from dataclasses import dataclass, field
from pathlib import Path

SONG_KINDS = ("intro", "verse", "chorus", "bridge", "outro")
FILM_KINDS = ("card", "talk", "view")
DEFAULT_FORM = "intro:8,verse:16,chorus:16,verse:16,chorus:16,bridge:8,chorus:16,outro:8"
# Tempo per song section, as a factor of the theme's bpm. Chorus pushes, the
# bridge and outro breathe. A theme overrides it with style["form_tempo"].
FORM_TEMPO = {"intro": 0.94, "verse": 1.0, "chorus": 1.05, "bridge": 0.92, "outro": 0.9}
BPM_RANGE = (40.0, 200.0)


class FormError(ValueError):
    pass


@dataclass
class FormSection:
    start: float
    end: float
    kind: str
    name: str = ""
    bars: int = 0
    bpm: float = 0.0  # 0 — the core decides from the kind and the theme
    theme: int = 1  # 1 or 2


@dataclass
class Form:
    length: float
    sections: list[FormSection]
    cuts: list[float] = field(default_factory=list)
    speech: list[tuple[float, float]] = field(default_factory=list)
    holes: list[tuple[float, float]] = field(default_factory=list)
    accents: list[float] = field(default_factory=list)
    key_audio: str | None = None
    theme2_for: list[str] = field(default_factory=list)
    tempo_map: list[tuple[float, float, float]] = field(default_factory=list)
    bed: dict | None = None
    demo: list[tuple[float, float]] = field(default_factory=list)
    source: str = ""

    @property
    def is_song(self) -> bool:
        return all(s.kind in SONG_KINDS for s in self.sections)

    def to_dict(self) -> dict:
        secs = [{k: v for k, v in s.__dict__.items() if k == "start" or v not in ("", 0, 0.0)} for s in self.sections]
        d: dict = {"length": self.length, "sections": secs}
        for k in ("cuts", "speech", "holes", "accents", "key_audio", "theme2_for", "tempo_map", "bed", "demo"):
            if getattr(self, k):
                d[k] = getattr(self, k)
        return d


def check_bpm(bpm: float, where: str) -> float:
    if not BPM_RANGE[0] <= bpm <= BPM_RANGE[1]:
        raise FormError(f"{where}: темп {bpm:g} вне {BPM_RANGE[0]:g}..{BPM_RANGE[1]:g}")
    return bpm


def load(path) -> Form:
    p = Path(path)
    try:
        d = json.loads(p.read_text())
    except (OSError, json.JSONDecodeError) as bad:
        raise FormError(f"{p}: {bad}") from bad
    secs = [
        FormSection(
            float(s["start"]),
            float(s["end"]),
            s["kind"],
            s.get("name", s.get("chapter", "")),
            int(s.get("bars", 0)),
            float(s.get("bpm", 0.0)),
            int(s.get("theme", 1)),
        )
        for s in d.get("sections", [])
    ]
    if not secs:
        raise FormError(f"{p}: в форме нет секций")
    for s in secs:
        if s.kind not in SONG_KINDS + FILM_KINDS:
            raise FormError(f"{p}: неизвестный вид секции «{s.kind}»")
        if s.end <= s.start:
            raise FormError(f"{p}: секция {s.start:g}–{s.end:g} пустая")
        if s.bpm:
            check_bpm(s.bpm, f"секция {s.name or s.kind}")
    tmap = [tuple(map(float, x)) for x in d.get("tempo_map", [])]
    for lo, hi, bpm in tmap:
        check_bpm(bpm, f"tempo_map {lo:g}-{hi:g}")
    key_audio = d.get("key_audio")
    if key_audio and not os.path.isabs(os.path.expanduser(key_audio)):
        key_audio = str(p.parent / key_audio)
    bed = d.get("bed")
    if bed and not os.path.isabs(os.path.expanduser(bed["path"])):
        bed = {**bed, "path": str(p.parent / bed["path"])}
    return Form(
        float(d.get("length", secs[-1].end)),
        secs,
        [float(x) for x in d.get("cuts", [])],
        [tuple(map(float, x)) for x in d.get("speech", [])],
        [tuple(map(float, x)) for x in d.get("holes", [])],
        [float(x) for x in d.get("accents", [])],
        key_audio,
        list(d.get("theme2_for", [])),
        tmap,
        bed,
        [tuple(map(float, x)) for x in d.get("demo", [])],
        str(p),
    )


def save(form: Form, path) -> None:
    Path(path).write_text(json.dumps(form.to_dict(), ensure_ascii=False, indent=1) + "\n")


# ---------- a song form without a plan ----------


def parse_spec(spec: str) -> list[tuple[str, int]]:
    """«intro:8,verse:16,…» → [(kind, bars)]; bars a positive multiple of 4."""
    out = []
    for part in (x.strip() for x in spec.split(",") if x.strip()):
        kind, _, bars = part.partition(":")
        kind = kind.strip().lower()
        if kind not in SONG_KINDS:
            raise FormError(f"--form: неизвестная секция «{kind}» (есть: {', '.join(SONG_KINDS)})")
        try:
            n = int(bars)
        except ValueError as bad:
            raise FormError(f"--form: «{part}» — нужно число тактов, «verse:16»") from bad
        if n <= 0 or n % 4:
            raise FormError(f"--form: «{part}» — тактов должно быть кратно 4")
        out.append((kind, n))
    if not out:
        raise FormError("--form пустая")
    return out


def tempo_of(kind: str, bpm: float, factors: dict | None = None) -> float:
    f = {**FORM_TEMPO, **(factors or {})}.get(kind, 1.0)
    return check_bpm(round(bpm * f, 2), f"секция {kind}")


def song_form(spec: list[tuple[str, int]], bpm: float, factors: dict | None = None) -> Form:
    """Sections on the time line, each at its own tempo; names count repeats."""
    check_bpm(bpm, "тема")
    t, secs, seen = 0.0, [], {}
    for kind, bars in spec:
        sb = tempo_of(kind, bpm, factors)
        seen[kind] = seen.get(kind, 0) + 1
        end = t + bars * 4 * 60.0 / sb
        secs.append(FormSection(round(t, 4), round(end, 4), kind, f"{kind} {seen[kind]}", bars, sb))
        t = end
    return Form(round(t, 4), secs, cuts=[s.start for s in secs[1:]], source="--form")


def spec_for_length(seconds: float, bpm: float, factors: dict | None = None) -> list[tuple[str, int]]:
    """The default form, scaled to `seconds`: bars in 4-bar blocks, a short
    piece drops the second verse/chorus pair and the bridge first."""
    if seconds < 20:
        raise FormError(f"--length {seconds:g}: короче 20 с формы не построить")
    full = parse_spec(DEFAULT_FORM)
    templates = [
        full,
        [("intro", 8), ("verse", 16), ("chorus", 16), ("bridge", 8), ("chorus", 16), ("outro", 8)],
        [("intro", 4), ("verse", 8), ("chorus", 8), ("verse", 8), ("chorus", 8), ("outro", 4)],
        [("intro", 4), ("verse", 8), ("chorus", 8), ("outro", 4)],
    ]

    def secs(sp):
        return song_form(sp, bpm, factors).length

    for tpl in templates:
        floor = [(k, 4) for k, _ in tpl]
        if secs(floor) <= seconds:
            break
    else:
        return [("intro", 4), ("chorus", 4), ("outro", 4)]
    scale = seconds / secs(tpl)
    sp = [(k, max(4, round(b * scale / 4) * 4)) for k, b in tpl]
    grow = [i for i, (k, _) in enumerate(sp) if k in ("chorus", "verse")]
    for step in range(64):  # ±4 bars on verses/choruses until within half a block
        cur = secs(sp)
        block = cur / sum(b for _, b in sp) * 4
        if abs(cur - seconds) <= block / 2:
            break
        i = grow[step % len(grow)]
        k, b = sp[i]
        if cur < seconds:
            sp[i] = (k, b + 4)
        elif b > 4:
            sp[i] = (k, b - 4)
    return sp
