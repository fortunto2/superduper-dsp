"""Музыкальное ядро sdsp-score: тема, мотив из MIDI, форма по плану, фактуры.

Всё здесь — чистые функции над нотами; звук, сведение и критика живут в
`score.py`. Вариант задаётся стилем темы (`style` в theme.json или
`--style`), а не правкой кода: кто ведёт мотив, кто играет остинато и
арпеджио, какие фактуры в ходу, насколько разгоняются виды.
"""

import itertools
import math
import random
import re
import struct
from dataclasses import dataclass, field
from pathlib import Path
from statistics import median

NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"]
FLATS = {"Db": "C#", "Eb": "D#", "Gb": "F#", "Ab": "G#", "Bb": "A#"}
SCALES = {False: [0, 2, 4, 5, 7, 9, 11], True: [0, 2, 3, 5, 7, 8, 10]}

# Роли партий. Инструмент роли — из стиля; пресет ставится ЯВНО (без него
# Wave играет `last.json`, «что крутили последним»).
MELODIC = {"lead", "arp", "ostinato"}
QUIET_VEL = 48
PAUSE = 1.5  # пауза речи короче — это вдох, а не пауза
LONG_TALK = 60.0  # после такой речи первая фраза видов — мотив в полный голос
THREAD_TALK = 45.0  # под речью длиннее — тихая нить мотива, чтобы тема не пропадала
EXEMPT = {"thread", "accent"}  # их громкость решена заранее — enforce их не режет
DEFAULT_STYLE = {
    "lead": ["wave", 14],  # Aulos (Odyssey) — флейта
    "ostinato": ["kubyz", 1],  # Bashkir Kubyz
    "arp": ["pad", 6],  # Pluck
    "pad": ["pad", 9],  # Cocteau Twins
    "bass": ["wave", 3],  # Triangle
    "pool": ["motif", "arp", "ostinato", "motif_seq", "motif_inv", "arp", "motif_retro", "motif_aug"],
    "talk_pool": ["bed", "halo", "drone", "bed", "halo"],
    "view_tempo": [1.12, 1.06, 1.15, 1.09],
    "talk_tempo": [0.88, 0.9],
    "energy": 1.0,
    "fills": True,  # ответ лида в паузах речи от 3 с
    "under_motif": "pad",  # что звучит под мотивом: pad / ostinato / arp
    "gains": {},  # дБ поверх громкости роли: {"lead": 2}
}
# Где лид звучит естественно: флейта сверху, кубыз — его родной низ.
REGISTER = {"wave": (60, 88), "kubyz": (50, 74), "pad": (55, 86)}


# ---------- тема ----------


def parse_chord(name: str) -> tuple[int, bool]:
    m = re.match(r"^([A-G])([#b]?)(m(?!aj))?", name.strip())
    if not m:
        raise ValueError(name)
    root = FLATS.get(m.group(1) + m.group(2), m.group(1) + m.group(2))
    if root not in NAMES:
        raise ValueError(name)
    return NAMES.index(root), bool(m.group(3))


def key_pc(name: str) -> int:
    name = FLATS.get(name, name)
    if name not in NAMES:
        raise ValueError(name)
    return NAMES.index(name)


@dataclass(eq=False)
class Theme:
    key: int
    minor: bool
    bpm: float
    chords: list[tuple[int, bool]]
    motif: list[tuple[int, float]]
    style: dict = field(default_factory=lambda: dict(DEFAULT_STYLE))
    name: str = "тема"

    @property
    def scale(self) -> list[int]:
        return SCALES[self.minor]

    @property
    def label(self) -> str:
        return f"{self.name} ({NAMES[self.key]}{'m' if self.minor else ''})"

    @classmethod
    def from_dict(cls, d: dict) -> "Theme":
        style = dict(DEFAULT_STYLE)
        style.update(d.get("style") or {})
        return cls(
            key=key_pc(d.get("key", "C")),
            minor=d.get("mode", "minor") == "minor",
            bpm=float(d.get("bpm", 72)),
            chords=[parse_chord(c) for c in d.get("chords", [])],
            motif=[(int(n["deg"]), float(n.get("beats", 1))) for n in d.get("motif", [])],
            style=style,
            name=d.get("name", "тема"),
        )

    def to_dict(self) -> dict:
        return {
            "name": self.name,
            "key": NAMES[self.key],
            "mode": "minor" if self.minor else "major",
            "bpm": self.bpm,
            "chords": [NAMES[r] + ("m" if mi else "") for r, mi in self.chords],
            "motif": [{"deg": d, "beats": b} for d, b in self.motif],
            "style": {k: v for k, v in self.style.items() if DEFAULT_STYLE.get(k) != v},
        }


def deg_to_midi(deg: int, tonic: int, scale: list[int]) -> int:
    o, r = divmod(deg - 1, 7)
    return tonic + 12 * o + scale[r]


def to_degree(pitch: int, tonic: int, scale: list[int]) -> int:
    o, r = divmod(pitch - tonic, 12)
    idx = min(range(7), key=lambda i: abs(scale[i] - r))
    if 12 - r < abs(scale[idx] - r):
        o, idx = o + 1, 0
    return 7 * o + idx + 1


def track_of(style: dict, role: str) -> str:
    inst, preset = style["lead" if role in EXEMPT else role]
    return f"{role}-{inst}{preset}"


def role_of(track: str) -> str:
    return track.split("-")[0]


# ---------- мотив из MIDI ----------


def _vlq(data: bytes, p: int) -> tuple[int, int]:
    n = 0
    while True:
        b = data[p]
        p += 1
        n = (n << 7) | (b & 0x7F)
        if not b & 0x80:
            return n, p


def _midi_events(data: bytes) -> tuple[int, list]:
    hlen, _fmt, ntrk, div = struct.unpack(">IHHH", data[4:14])
    pos, events = 8 + hlen, []
    for _ in range(ntrk):
        ln = struct.unpack(">I", data[pos + 4 : pos + 8])[0]
        p, end, tick, status = pos + 8, pos + 8 + ln, 0, 0
        while p < end:
            delta, p = _vlq(data, p)
            tick += delta
            b = data[p]
            if b == 0xFF:
                typ = data[p + 1]
                ln2, p = _vlq(data, p + 2)
                if typ == 0x51:
                    events.append((tick, 0, int.from_bytes(data[p : p + 3], "big"), 0))
                p += ln2
                continue
            if b in (0xF0, 0xF7):
                ln2, p = _vlq(data, p + 1)
                p += ln2
                continue
            if b & 0x80:
                status, p = b, p + 1
            hi = status & 0xF0
            one = hi in (0xC0, 0xD0)
            a, c = data[p], (0 if one else data[p + 1])
            p += 1 if one else 2
            if hi == 0x90 and c > 0:
                events.append((tick, 2, a, c))
            elif hi in (0x80, 0x90):
                events.append((tick, 1, a, 0))
        pos = end
    return div, sorted(events)


def read_midi(path) -> list[tuple[float, float, int, int]]:
    """[(начало_с, длина_с, нота, vel)] — свой разбор SMF, без зависимостей."""
    data = Path(path).read_bytes()
    if data[:4] != b"MThd":
        raise ValueError(f"{path} — не MIDI")
    div, events = _midi_events(data)
    tempo, last, sec, on, notes = 500000, 0, 0.0, {}, []
    for tick, kind, a, c in events:
        sec += (tick - last) * tempo / 1e6 / div
        last = tick
        if kind == 0:
            tempo = a
        elif kind == 2:
            on.setdefault(a, []).append((sec, c))
        elif on.get(a):
            s, v = on[a].pop(0)
            notes.append((s, sec - s, a, v))
    return sorted(notes)


def top_cluster(notes, gap=5, share=0.3):
    """Мелодия над басом: если высоты делятся пустой полосой от `gap` полутонов,
    берём верхний кластер, несущий хотя бы `share` звука самого тяжёлого.
    Без этого «Башҡорт сәй эсә» отдавал басовую линию E2–A2 вместо напева."""
    if not notes:
        return []
    w: dict[int, float] = {}
    for _, d, p, v in notes:
        w[p] = w.get(p, 0.0) + d * v
    ps = sorted(w)
    groups = [[ps[0]]]
    for p in ps[1:]:
        if p - groups[-1][-1] > gap:
            groups.append([p])
        else:
            groups[-1].append(p)
    weights = [sum(w[p] for p in g) for g in groups]
    top = max(weights)
    pick = next(g for g, x in reversed(list(zip(groups, weights))) if x >= share * top)
    return [n for n in notes if pick[0] <= n[2] <= pick[-1]]


def melody_line(notes) -> list[tuple[float, int]]:
    """Один голос из расшифровки: полоса, где больше всего звука, в кластере —
    самая звучная нота, октавные ошибки Basic Pitch сложены к соседке."""
    notes = top_cluster([n for n in notes if n[1] >= 0.08])
    if not notes:
        return []
    pitches = [n[2] for n in notes]

    def weight(lo):
        return sum(d * v for _, d, p, v in notes if lo <= p <= lo + 15)

    lo = max(range(min(pitches), max(pitches) + 1), key=lambda x: (weight(x), x))
    band = sorted((t, p, d * v) for t, d, p, v in notes if lo <= p <= lo + 15)
    raw: list[list] = []
    for t, p, w in band:
        if raw and t - raw[-1][0] < 0.07:
            if w > raw[-1][2]:
                raw[-1] = [raw[-1][0], p, w]
            continue
        raw.append([t, p, w])
    center = median(p for _, p, _ in raw)
    line: list[tuple[float, int]] = []
    for t, p, _ in raw:
        prev = line[-1][1] if line else center
        best = min((p + 12 * k for k in (-2, -1, 0, 1, 2)), key=lambda q: abs(q - prev) + 0.5 * abs(q - center))
        line.append((t, best))
    return line


def motif_from_midi(path, key_pc: int, minor: bool, info: dict | None = None) -> list[tuple[int, float]]:
    """Самая повторяющаяся фигура 4–8 нот, в ступенях лада, с ритмом в долях.

    Если ни одна фигура не повторилась (редкая расшифровка: у «сәй эсә» 51 нота
    на 50 с), берётся самая богатая фраза 5–8 нот — и `info["how"]` это говорит.
    """
    info = info if info is not None else {}
    line = melody_line(read_midi(path))
    if len(line) < 8:
        return []
    scale = SCALES[minor]
    mid = median(p for _, p in line)
    tonic = max(m for m in range(128) if m % 12 == key_pc and m <= mid)
    degs = [to_degree(p, tonic, scale) for _, p in line]
    best = None
    for n in range(8, 3, -1):
        found: dict[tuple, list[int]] = {}
        for i in range(len(line) - n + 1):
            if any(line[i + k + 1][0] - line[i + k][0] > 1.2 for k in range(n - 1)):
                continue
            key = tuple(degs[i : i + n])
            if len(set(key)) < 3:
                continue
            starts = found.setdefault(key, [])
            if not starts or starts[-1] + n <= i:
                starts.append(i)
        for key, starts in found.items():
            score = (len(starts) * n, n)
            if len(starts) >= 2 and (best is None or score > best[0]):
                best = (score, key, starts)
    if best is None:
        for n in range(8, 4, -1):
            for i in range(len(line) - n + 1):
                if any(line[i + k + 1][0] - line[i + k][0] > 1.2 for k in range(n - 1)):
                    continue
                key = tuple(degs[i : i + n])
                score = (len(set(key)), n)
                if best is None or score > best[0]:
                    best = (score, key, [i])
        if best is None or best[0][0] < 3:
            info["how"] = "нет фразы"
            return []
        info["how"] = f"без повторов — самая богатая фраза, {best[0][0]} ступеней"
    else:
        info["how"] = f"повтор ×{len(best[2])}"
    _, key, starts = best
    n = len(key)
    iois = [median(line[s + k + 1][0] - line[s + k][0] for s in starts) for k in range(n - 1)]
    unit = median(iois)
    beats = [min(2.0, max(0.5, round(x / unit * 2) / 2)) for x in iois] + [1.0]
    shift = 7 * ((key[0] - 1) // 7)
    return [(d - shift, b) for d, b in zip(key, beats)]


# ---------- форма ----------


def merge_spans(spans, gap: float) -> list[tuple[float, float]]:
    out: list[tuple[float, float]] = []
    for lo, hi in sorted(spans):
        if out and lo - out[-1][1] < gap:
            out[-1] = (out[-1][0], max(out[-1][1], hi))
        else:
            out.append((lo, hi))
    return out


@dataclass(eq=False)
class Section:
    start: float
    end: float
    kind: str  # фильм: card / talk / view; песня: intro / verse / chorus / bridge / outro
    chapter: str
    bpm: float = 0.0
    theme: Theme | None = None

    @property
    def length(self) -> float:
        return self.end - self.start


@dataclass(eq=False)
class Phrase:
    start: float
    end: float
    beat: float
    texture: str
    var: int
    section: Section
    intensity: float = 1.0
    changes: list = field(default_factory=list)  # [(t, (root_pc, minor))]
    note: str = ""
    layers: list = field(default_factory=list)  # ["thread"]
    lift: dict = field(default_factory=dict)  # роль → дБ на время фразы
    drums: str = ""  # форма: none / soft / light / half / full; "" — по фактуре (план)

    @property
    def label(self) -> str:
        return "+".join([self.texture] + self.layers)

    @property
    def bar(self) -> float:
        return self.beat * 4

    @property
    def theme(self) -> Theme:
        assert self.section.theme is not None
        return self.section.theme


def parse_tempo_map(spec: str) -> list[tuple[float, float, float]]:
    out = []
    for part in (x.strip() for x in (spec or "").split(",") if x.strip()):
        rng, _, bpm = part.partition(":")
        lo, _, hi = rng.partition("-")
        out.append((float(lo), float(hi), float(bpm)))
    return out


def assign_themes(sections, theme, theme2=None, theme2_for=()) -> None:
    """Вторая тема берёт секции по виду (`view`, `card`, `talk`) или по префиксу главы."""
    for s in sections:
        hit = theme2 is not None and any(s.kind == w or s.chapter.startswith(w) for w in theme2_for)
        s.theme = theme2 if hit else theme


def assign_tempo(sections, tempo_map) -> None:
    count: dict[tuple, int] = {}
    for s in sections:
        th = s.theme
        assert th is not None
        mid = (s.start + s.end) / 2
        hit = [b for lo, hi, b in tempo_map if lo <= mid < hi]
        key = (id(th), s.kind)
        n = count.get(key, 0)
        count[key] = n + 1
        if hit:
            if not 40 <= hit[0] <= 200:  # «:0» делил на ноль в make_phrases
                raise ValueError(f"темп {hit[0]:g} в tempo_map вне 40..200")
            s.bpm = hit[0]
        elif s.kind in ("view", "talk"):
            f = th.style[f"{s.kind}_tempo"]
            s.bpm = th.bpm * max(0.85, min(1.15, f[n % len(f)]))
        else:
            s.bpm = th.bpm


class Chooser:
    """Фактура фразы: не повторять две последние, чаще брать то, что звучало реже."""

    def __init__(self):
        self.used: dict[str, int] = {}
        self.last: list[str] = []
        self.chapter_seen: set[str] = set()
        self.n = 0
        self.nt = 0

    def pick(self, sec: Section, bars: float, first_in_section: bool, force=None) -> str:
        style = sec.theme.style if sec.theme else DEFAULT_STYLE
        if force:
            tex = force
        elif sec.kind == "card":
            tex = "card"
        elif sec.kind == "talk":
            pool = [t for t in style["talk_pool"] if t not in self.last[-1:]] or style["talk_pool"]
            tex = pool[self.nt % len(pool)]
            self.nt += 1
        elif sec.chapter not in self.chapter_seen and "motif" not in self.last[-1:]:
            tex = "motif"
        elif not first_in_section and self.n % 4 == 3 and bars >= 4 and self.last[-1] != "rest":
            tex = "rest"
        else:
            pool = style["pool"]
            recent = set(self.last[-2:])
            cands = [t for t in pool if t not in recent] or pool
            k = self.n % len(cands)
            tex = min(cands[k:] + cands[:k], key=lambda t: self.used.get(t, 0))
        if sec.kind == "view":
            self.chapter_seen.add(sec.chapter)
            self.n += 1
        self.used[tex] = self.used.get(tex, 0) + 1
        self.last.append(tex)
        return tex


def make_phrases(sections, cuts) -> list[Phrase]:
    chooser, out, ci = Chooser(), [], {}
    chapters: dict[str, tuple[float, float]] = {}
    for s in sections:
        lo, hi = chapters.get(s.chapter, (s.start, s.end))
        chapters[s.chapter] = (min(lo, s.start), max(hi, s.end))
    lengths = [8, 6, 8, 4]
    prev = None
    for s in sections:
        after_long = prev is not None and prev.kind == "talk" and prev.length >= LONG_TALK
        prev = s
        beat = 60.0 / s.bpm
        bar = beat * 4
        t, k = s.start, 0
        while s.end - t > 0.05:
            left = (s.end - t) / bar
            want = lengths[(len(out) + k) % len(lengths)] if s.kind == "view" else 8
            force = "motif" if after_long and k == 0 and s.kind == "view" else None
            tex = chooser.pick(s, left, k == 0, force)
            if tex == "rest":
                want = 2
            n = left if left < want + 2 else want
            end = min(s.end, t + n * bar)
            var = chooser.used[tex] - 1
            lo, hi = chapters[s.chapter]
            pos = ((t + end) / 2 - lo) / max(1e-6, hi - lo)
            ph = Phrase(t, end, beat, tex, var, s, 0.8 + 0.25 * math.sin(math.pi * pos))
            if force:
                ph.intensity, ph.lift, ph.note = 1.1, {"pad": 4.0}, "после длинной речи"
            if s.kind == "talk" and s.length >= THREAD_TALK and k >= 1:
                ph.layers.append("thread")
            hr = 2 if tex in ("bed", "drone", "halo", "motif_aug") or var % 3 == 2 else 1
            th = ph.theme
            tc = t
            while tc < end - beat * 0.5:
                i = ci.get(id(th), 0)
                ph.changes.append((tc, th.chords[i % len(th.chords)]))
                ci[id(th)] = i + 1
                tc += hr * bar
            snap_changes(ph, cuts)
            out.append(ph)
            t, k = end, k + 1
    modulate(out)
    return out


def modulate(phrases) -> None:
    """Смена темы — через доминанту новой тональности в последнем такте перед ней."""
    for prev, ph in itertools.pairwise(phrases):
        if prev.theme is ph.theme or not prev.changes:
            continue
        dom = ((ph.theme.key + 7) % 12, False)
        at = max(prev.start, prev.end - prev.bar)
        prev.changes = [c for c in prev.changes if c[0] < at - 1e-6] + [(at, dom)]
        if ph.changes:
            ph.changes[0] = (ph.changes[0][0], (ph.theme.key, ph.theme.minor))
        ph.note = f"модуляция → {ph.theme.label}"


def snap_changes(ph: Phrase, cuts) -> None:
    """Аккорд на доле, ближайшей к склейке внутри фразы."""
    for c in cuts:
        if not ph.start + ph.beat < c < ph.end - ph.beat:
            continue
        bt = ph.start + round((c - ph.start) / ph.beat) * ph.beat
        j = min(range(1, len(ph.changes)), key=lambda i: abs(ph.changes[i][0] - bt), default=None)
        if j is None or abs(ph.changes[j][0] - bt) > 2 * ph.beat + 1e-9:
            continue
        prev_t = ph.changes[j - 1][0]
        next_t = ph.changes[j + 1][0] if j + 1 < len(ph.changes) else ph.end
        if prev_t < bt < next_t:
            ph.changes[j] = (bt, ph.changes[j][1])


# ---------- фактуры ----------


class Writer:
    def __init__(self, seed: int, speech):
        self.rng = random.Random(seed)
        self.notes: dict[str, list] = {}
        self.speech = speech

    def vel(self, ph, t, base, accent=False):
        pos = (t - ph.start) / max(1e-6, ph.end - ph.start)
        v = base * ph.intensity * ph.theme.style["energy"] * (0.9 + 0.15 * math.sin(math.pi * pos))
        v += (7 if accent else 0) + self.rng.uniform(-4, 4)
        return int(max(20, min(118, round(v))))

    def add(self, role, at, dur, pitch, vel, ph):
        dur = min(dur, ph.end + 2.5 - at)
        if dur > 0.04 and at < ph.end:
            track = track_of(ph.theme.style, role)
            self.notes.setdefault(track, []).append([round(at, 4), round(dur, 4), int(pitch), int(vel)])

    @staticmethod
    def segments(ph):
        ch = ph.changes or [(ph.start, (ph.theme.key, ph.theme.minor))]
        return [(t, ch[i + 1][0] if i + 1 < len(ch) else ph.end, c) for i, (t, c) in enumerate(ch)]

    @staticmethod
    def chord_at(ph, t):
        c = ph.changes[0][1]
        for tc, cc in ph.changes:
            if tc <= t + 1e-9:
                c = cc
        return c

    @staticmethod
    def triad(chord, base=57, inv=0):
        root, minor = chord
        r = base + (root - base) % 12
        tones = [r, r + (3 if minor else 4), r + 7]
        for _ in range(inv % 3):
            tones = tones[1:] + [tones[0] + 12]
        return tones

    def lead_tonic(self, ph) -> int:
        lo, _hi = REGISTER.get(ph.theme.style["lead"][0], (60, 88))
        return lo + (ph.theme.key - lo) % 12

    def bass(self, ph, base_vel, octave=0, sparse=False):
        for a, b, (root, _) in self.segments(ph):
            dur = ph.beat * 1.5 if sparse else b - a + 0.3
            self.add("bass", a, dur, 36 + root % 12 + octave, self.vel(ph, a, base_vel, True), ph)

    def pad(self, ph, base_vel, inv=0, base=55):
        for a, b, chord in self.segments(ph):
            for p in self.triad(chord, base, inv):
                self.add("pad", a, b - a + 0.6, p, self.vel(ph, a, base_vel), ph)

    def motif_line(self, ph, degs, t, base_vel, octave=0, stretch=1.0, limit=None, role="lead"):
        """Одна проводка мотива; возвращает время конца."""
        end = limit or ph.end
        lo, hi = REGISTER.get(ph.theme.style["lead"][0], (60, 88))
        tonic = self.lead_tonic(ph)
        for i, (d, beats) in enumerate(degs):
            dur = beats * ph.beat * stretch
            if t >= end - 0.05:
                break
            pitch = deg_to_midi(d, tonic, ph.theme.scale) + octave
            while pitch > hi:
                pitch -= 12
            while pitch < lo:
                pitch += 12
            self.add(role, t, min(dur * 0.95, end - t), pitch, self.vel(ph, t, base_vel, i == 0), ph)
            t += dur
        return t

    def statements(self, ph, variants, base_vel, octave=0, stretch=1.0):
        """Проводки по тактам с вдохом между ними; variants(k) → ступени k-й проводки."""
        k, t = 0, ph.start
        while t < ph.end - ph.beat:
            degs = variants(k)
            length = sum(b for _, b in degs) * stretch
            self.motif_line(ph, degs, t, base_vel, octave, stretch)
            t += math.ceil((length + 1) / 4) * ph.bar
            k += 1

    def fills(self, ph):
        """В паузе речи от 1.5 с лид отвечает головой мотива — тихо."""
        if not ph.theme.style.get("fills"):
            return
        sp = [(a, b) for a, b in self.speech if b > ph.start and a < ph.end]
        edges = [ph.start] + [x for a, b in sp for x in (a, b)] + [ph.end]
        for lo, hi in zip(edges[::2], edges[1::2]):
            if hi - lo >= PAUSE:
                self.motif_line(ph, ph.theme.motif[:4], lo + 0.25, 64, limit=hi - 0.25)

    def thread(self, ph):
        """Нить: ноты мотива по одной-две на такт, длинно и тихо, в низком регистре
        лида. Слышно, что тема никуда не ушла, и она не спорит со словами."""
        m, i, t = ph.theme.motif, ph.var * 3, ph.start
        per = 1 if ph.var % 2 == 0 else 2
        while t < ph.end - 0.5:
            for j in range(per):
                d = m[(i + j) % len(m)][0]
                at = t + j * ph.bar / per
                self.motif_line(ph, [(d, 4.0 / per)], at, 40, octave=-12, limit=ph.end, role="thread")
            i += per
            t += ph.bar


def motif_variants(theme: Theme, kind: str, var: int):
    m = theme.motif
    first = m[0][0]
    inv = [(2 * first - d, b) for d, b in m]
    retro = list(zip([d for d, _ in reversed(m)], [b for _, b in m]))
    resolved = m[:-1] + [(1 if m[-1][0] < 5 else 8, max(2.0, m[-1][1]))]
    step = 1 if var % 2 == 0 else -1
    if kind == "motif":
        return lambda k: m if k % 2 == 0 else resolved
    if kind == "motif_seq":
        return lambda k: [(d + step * k, b) for d, b in m]
    if kind == "motif_inv":
        return lambda k: inv if k % 2 == 0 else m
    if kind == "motif_retro":
        return lambda k: retro if k % 2 == 0 else [(2 * retro[0][0] - d, b) for d, b in retro]
    return lambda k: m if k % 2 == 0 else inv


ARP_PATTERNS = [[0, 1, 2, 3], [0, 1, 2, 3, 2, 1], [0, 2, 1, 3], [3, 2, 1, 0, 1, 2]]
OSTINATO_PATTERNS = [
    [
        (0, 0.75, 0, 1),
        (0.75, 0.25, 0, 0),
        (1, 0.5, 7, 0),
        (1.5, 0.5, 0, 0),
        (2, 0.75, 0, 1),
        (2.75, 0.25, 0, 0),
        (3, 1, 12, 0),
    ],
    [
        (0, 0.5, 0, 1),
        (0.5, 0.5, 0, 0),
        (1, 0.5, 0, 0),
        (1.5, 0.5, 10, 0),
        (2, 0.5, 0, 1),
        (2.5, 0.5, 7, 0),
        (3, 0.5, 0, 0),
        (3.5, 0.5, 5, 0),
    ],
    [(0, 1.5, 0, 1), (1.5, 0.5, 7, 0), (2, 1, 0, 1), (3, 0.25, 0, 0), (3.25, 0.25, 0, 0), (3.5, 0.5, 12, 0)],
]


def write_talk(w: Writer, ph: Phrase) -> None:
    tex, var = ph.texture, ph.var
    if tex == "bed":
        w.bass(ph, 44, octave=12 * (var % 2))
        w.pad(ph, 40, inv=var)
    elif tex == "halo":
        w.pad(ph, 36, inv=var % 2, base=57)
    elif tex == "drone":
        w.add(
            "bass", ph.start, ph.end - ph.start + 0.5, 36 + ph.theme.key + 12 * (var % 2), w.vel(ph, ph.start, 42), ph
        )
    if "thread" in ph.layers:
        w.thread(ph)
    w.fills(ph)


def write_arp(w: Writer, ph: Phrase, base=66, layer=False) -> None:
    """Арпеджио; `layer` — тихий слой под мотивом, без своего баса и головы мотива."""
    var = ph.var
    pat = ARP_PATTERNS[var % len(ARP_PATTERNS)]
    step = [0.5, 1 / 3, 0.25 if ph.beat >= 0.75 else 0.5][var % 3]
    if not layer:
        w.bass(ph, 70, sparse=var % 2 == 1)
    t, i = ph.start, 0
    while t < ph.end - 0.05:
        tones = w.triad(w.chord_at(ph, t), 57 + 12 * (var % 2), inv=var // 2)
        tones = tones + [tones[0] + 12]
        w.add(
            "arp",
            t,
            step * ph.beat * 1.8,
            tones[pat[i % len(pat)] % len(tones)],
            w.vel(ph, t, base, i % len(pat) == 0),
            ph,
        )
        t += step * ph.beat
        i += 1
    if var % 2 and not layer:
        w.motif_line(ph, ph.theme.motif[:3], ph.end - 2 * ph.bar, 64)


def write_ostinato(w: Writer, ph: Phrase, base=72, layer=False) -> None:
    var = ph.var
    pat = OSTINATO_PATTERNS[var % len(OSTINATO_PATTERNS)]
    pedal = var % 2 == 0
    bt = ph.start
    while bt < ph.end - 0.05:
        root = ph.theme.key if pedal else w.chord_at(ph, bt)[0]
        for off, dur, iv, acc in pat:
            at = bt + off * ph.beat
            w.add("ostinato", at, dur * ph.beat * 0.9, 48 + (root - 48) % 12 + iv, w.vel(ph, at, base, bool(acc)), ph)
        bt += ph.bar
    if layer:
        return
    w.bass(ph, 62, sparse=True)
    if var % 3 == 1:
        w.pad(ph, 44, inv=var)


def write_phrase(w: Writer, ph: Phrase) -> None:
    tex, var = ph.texture, ph.var
    if tex == "rest":
        return
    if tex in ("bed", "halo", "drone"):
        write_talk(w, ph)
    elif tex == "card":
        w.pad(ph, 58, inv=var)
        w.bass(ph, 68)
        w.motif_line(ph, ph.theme.motif, ph.start, 80, octave=-12 if var % 2 else 0)
    elif tex == "arp":
        write_arp(w, ph)
    elif tex in ("ostinato", "kubyz"):
        write_ostinato(w, ph)
    else:
        octave = (0 if var % 2 == 0 else -12) if tex == "motif" else [0, -12, 0, 12][var % 4]
        stretch = 2.0 if tex == "motif_aug" else 1.0
        w.statements(ph, motif_variants(ph.theme, tex, var), 84, octave, stretch)
        w.bass(ph, 64, octave=12 if tex == "motif_aug" else 0)
        under = ph.theme.style.get("under_motif", "pad")
        if under == "ostinato":  # мотив поверх остинато — подпись варианта
            write_ostinato(w, ph, 56, layer=True)
            w.pad(ph, 40, inv=var)
        elif under == "arp":
            write_arp(w, ph, 50, layer=True)
        elif tex in ("motif", "motif_aug") or var % 2 == 0:
            w.pad(ph, 46, inv=var)
        else:
            for a, _b, chord in w.segments(ph):
                for p in w.triad(chord, 60, var):
                    w.add("arp", a, ph.beat * 1.5, p, w.vel(ph, a, 54), ph)


# ---------- тишина и речь ----------


def _cut(a, b, v, lo, hi, quiet_vel):
    """Кусок [a,b) без [lo,hi); quiet_vel — вместо дырки сыграть тише."""
    if b <= lo or a >= hi:
        return [(a, b, v)]
    out = []
    if a < lo:
        out.append((a, lo, v))
    if quiet_vel is not None:
        out.append((max(a, lo), min(b, hi), quiet_vel))
    if b > hi:
        out.append((hi, b, v))
    return out


def enforce(notes, speech, holes, pad=0.2):
    """Под речью мелодия молчит, бас/подушка тихие; в дырке — ничего."""
    sp = [(a - pad, b + pad) for a, b in speech]
    out = {}
    for track, ns in notes.items():
        melodic = role_of(track) in MELODIC
        free = role_of(track) in EXEMPT
        res = []
        for at, dur, p, v in ns:
            pieces = [(at, at + dur, v)]
            for lo, hi in holes:
                pieces = [q for a, b, vv in pieces for q in _cut(a, b, vv, lo, hi, None)]
            for lo, hi in [] if free else sp:
                pieces = [
                    q for a, b, vv in pieces for q in _cut(a, b, vv, lo, hi, None if melodic else min(vv, QUIET_VEL))
                ]
            res += [[round(a, 4), round(b - a, 4), p, vv] for a, b, vv in pieces if b - a > (0.15 if vv == v else 1.0)]
        out[track] = sorted(res)
    return out


# ---------- сборка и метрика ----------


@dataclass
class Score:
    length: float
    sections: list[Section]
    phrases: list[Phrase]
    notes: dict[str, list]
    speech: list[tuple[float, float]]
    holes: list[tuple[float, float]]
    speech_src: str = ""
    lifts: list = field(default_factory=list)  # [(роль, начало, конец, дБ)]


def motif_fit(motif, theme: Theme, ref_notes) -> float:
    """Pearson звуковысотных профилей (12 классов, по длительности) мотива и
    эталона — нот вокала [{midi, dur}] из pitch_line.py --json. Без эталона nan."""
    if not ref_notes or not motif:
        return float("nan")
    a, b = [0.0] * 12, [0.0] * 12
    for d, beats in motif:
        a[deg_to_midi(d, 60 + theme.key, theme.scale) % 12] += beats
    for n in ref_notes:
        b[int(n["midi"]) % 12] += float(n.get("dur", 1.0))
    ma, mb = sum(a) / 12, sum(b) / 12
    cov = sum((x - ma) * (y - mb) for x, y in zip(a, b))
    va = math.sqrt(sum((x - ma) ** 2 for x in a))
    vb = math.sqrt(sum((y - mb) ** 2 for y in b))
    return cov / (va * vb) if va and vb else float("nan")


def bars_of(score: Score) -> list[tuple[float, float, float]]:
    out = []
    for ph in score.phrases:
        t = ph.start
        while t < ph.end - 1e-6:
            out.append((t, min(ph.end, t + ph.bar), ph.beat))
            t += ph.bar
    return out


def silent_gaps(score: Score, longest=20.0) -> list[tuple[float, float]]:
    """Куски длиннее `longest` без единой звучащей ноты, кроме заказанных дырок."""
    spans = merge_spans([(at, at + d) for ns in score.notes.values() for at, d, _, _ in ns] + list(score.holes), 0.0)
    edges = [0.0] + [x for a, b in spans for x in (a, b)] + [score.length]
    return [(a, b) for a, b in zip(edges[::2], edges[1::2]) if b - a > longest]


def diversity(score: Score) -> dict:
    import bisect

    allnotes = sorted((at, tr, dur, p) for tr, ns in score.notes.items() for at, dur, p, _ in ns)
    starts = [n[0] for n in allnotes]
    bars = bars_of(score)
    fps = []
    for i in range(0, len(bars) - 7, 8):
        fp = []
        for bi, (t0, t1, beat) in enumerate(bars[i : i + 8]):
            for at, tr, dur, p in allnotes[
                bisect.bisect_left(starts, t0 - 1e-6) : bisect.bisect_left(starts, t1 - 1e-6)
            ]:
                fp.append((tr, bi, round((at - t0) / beat * 4) / 4, p, round(dur / beat * 4) / 4))
        fps.append(tuple(sorted(fp)))
    rep = sum(1 for i in range(1, len(fps)) if fps[i] == fps[i - 1])
    seen_rep = sum(1 for i in range(len(fps)) if fps[i] in set(fps[:i]))
    tex = [p.texture for p in score.phrases]
    return {
        "windows": len(fps),
        "repeat_share": rep / max(1, len(fps) - 1),
        "any_repeat_share": seen_rep / max(1, len(fps)),
        "texture_changes": sum(1 for i in range(1, len(tex)) if tex[i] != tex[i - 1]),
        "textures": sorted(set(tex)),
        "tempi": sorted({round(s.bpm) for s in score.sections}),
        "gaps": silent_gaps(score),
    }
