"""Ритм-секция и низ для sdsp-score: ударные из семплов, 808-саб, готовый бит.

Фидбек Рустама на v4 (13 Oct 2026): «ощущение, что не считывается. Может,
bpm не тот. Нет басов? В папке Music есть SuperDuper Samples, там барабаны».
Тема без пульса и без низа не держит фильм, поэтому здесь:

- **Ударные из его семплов** (`kit` в style): `kubyz` — кит из щипков кубыза,
  `cassette` — Cassette808, `909` — TR909. Паттерн зависит от фактуры фразы:
  под речью — ничего или редкая мягкая бочка на сильную долю (vel 40), на
  видах — бочка + хэт + рим/клэп с лёгким свингом, перед карточкой и сменой
  темы — филл томами.
- **`feel: double`** — хэты в двойном времени (шестнадцатые при 72 = восьмые
  при 144, исходном темпе «Тормош»), бочка и бэкбит остаются на 72.
- **808-саб** (RUST 808 Sub, транспонирован ресемплом до тоники аккорда)
  звучит на каждой бочке, держится до следующей.
- **Готовый бит** (`--bed`): флип лупом под виды той же тональности,
  растянут rubberband'ом до темпа секции; в таких секциях синтетических
  ударных и саба нет — бит их заменяет.

Звук пишется здесь же (numpy + ffmpeg) с фиксированной громкостью, и стемы
входят в микс sdsp-chain наравне с остальными. Почему не сэмплер sdsp-chain:
его `Sample` — индекс 0..255 в отсортированном банке, а в SuperDuper Samples
581 wav (кит кубыза на 345-м месте), и рендер не называет, какой файл сыграл —
неверный индекс прозвучал бы молча.
"""

import json
import random
import subprocess
from pathlib import Path

from . import config

SR = 48000
VIEW_TEX = {"motif", "motif_seq", "motif_inv", "motif_retro", "arp", "ostinato"}
TALK_TEX = {"bed", "halo", "drone"}
# (бочки, бэкбит) в долях такта: три грува, чтобы соседние фразы не совпадали
GROOVES = [([0.0, 2.5], [1.0, 3.0]), ([0.0, 0.75, 2.5], [1.0, 3.0]), ([0.0, 2.0, 2.75], [3.0])]


def kits() -> dict:
    return config.cfg()["kits"]


def sub_files() -> dict[int, Path]:
    s = config.cfg()["sub808"]
    return {int(k): config.samples_dir() / s["dir"] / s["pattern"].format(v) for k, v in s["notes"].items()}


def preflight(hits, subs) -> str | None:
    """Всё, что понадобится рендеру ударных, — до первого стема: неизвестный кит,
    нет семпла. Ответ — текст отказа или None."""
    for kit, part in sorted({tuple(name.split(":")) for _, name, _ in hits}):
        if kit not in kits():
            return f"неизвестный кит «{kit}» (есть: {', '.join(sorted(kits()))})"
        f = config.samples_dir() / kits()[kit][part]
        if not f.exists():
            return f"нет семпла {f} (кит {kit}, партия {part}; SDSP_SAMPLES?)"
    if subs:
        for f in sub_files().values():
            if not f.exists():
                return f"нет семпла 808-саба {f} (SDSP_SAMPLES?)"
    return None


# ---------- ноты ударных (чистые функции) ----------


def _swing(off: float, step: float, amount: float) -> float:
    """Нечётные доли сетки `step` опаздывают на `amount` шага."""
    k = round(off / step)
    return off + (amount * step if k % 2 == 1 else 0.0)


def phrase_hits(ph, nxt, rng: random.Random) -> list[tuple[float, str, int]]:
    """[(секунда, партия, vel)] одной фразы; `nxt` — следующая фраза или None."""
    st = ph.theme.style
    if not st.get("drums", True):
        return []
    tex, var, beat = ph.texture, ph.var, ph.beat
    out: list[tuple[float, str, int]] = []
    gain = ph.intensity * st.get("energy", 1.0)

    def hit(t, part, v):
        if ph.start - 1e-6 <= t < ph.end - 0.02:
            out.append((round(t, 4), part, int(max(1, min(127, v * gain + rng.uniform(-5, 5))))))

    bars = []
    t = ph.start
    while t < ph.end - 0.05:
        bars.append(t)
        t += ph.bar
    if ph.drums == "none":
        return out
    if tex in TALK_TEX or ph.drums == "soft":
        if var % 2 == 0 or ph.drums == "soft":  # редкая мягкая бочка на сильную долю
            for i, b in enumerate(bars):
                if i % 2 == 0:
                    hit(b, "soft", 40)
        return out
    if tex == "card":
        hit(ph.start, "crash", 70)
        n = max(4, int((ph.end - ph.start) / (beat / 4)))
        for i in range(n):  # рулон томов, нарастает
            hit(ph.start + beat + i * beat / 4, "tom", 40 + 50 * i / n)
        return out
    if tex == "rest":
        return out
    double = st.get("feel", "double") == "double"
    hstep = 0.25 if double else 0.5
    swing = float(st.get("swing", 0.12))
    kicks, backs = GROOVES[var % len(GROOVES)]
    half = tex == "motif_aug" or ph.drums == "half"
    if half:
        kicks, backs, hstep = [0.0], [2.0], 1.0
    light = tex == "ostinato" or ph.drums == "light"  # кубыз сам ритмичен — ударные легче
    if ph.drums == "full" and ph.start <= ph.section.start + 1e-6:
        hit(ph.start, "crash", 74)  # припев открывается тарелкой
    for i, b in enumerate(bars):
        for k in kicks[:2] if light else kicks:
            hit(b + k * beat, "kick", 96 if k == 0 else 78)
        for k in backs:
            hit(b + k * beat, "rim" if (var + i) % 3 else "clap", 74)
        if i == 0 and var % 2 == 0 and not half:
            continue  # вход фразы без хэтов — слышно начало
        off = 0.0
        while off < 4.0 - 1e-6:
            if not (light and off % 1.0 == 0.0):
                v = 66 if off % 1.0 == 0 else (52 if off % 0.5 == 0 else 40)
                hit(b + _swing(off, hstep, swing) * beat, "hat", v)
            off += hstep
        if var % 3 == 1 and i % 2 == 1:
            hit(b + 3.5 * beat, "open", 50)
    into_chorus = ph.drums and nxt is not None and nxt.section is not ph.section and nxt.section.kind == "chorus"
    if nxt is not None and (nxt.texture == "card" or nxt.theme is not ph.theme or into_chorus) and bars:
        last = bars[-1]  # филл перед карточкой и сменой темы
        out = [h for h in out if h[0] < last + 2 * beat]
        for j in range(8):
            hit(last + 2 * beat + j * beat / 4, "tom", 45 + 6 * j)
    return out


def sub_notes(hits, phrases) -> list[tuple[float, float, int, int]]:
    """808-саб на каждой бочке: тоника аккорда в первой октаве, до следующей бочки."""
    kicks = sorted((t, v) for t, part, v in hits if part.split(":")[-1] in ("kick", "soft"))
    out = []
    for i, (t, v) in enumerate(kicks):
        ph = next((p for p in phrases if p.start - 1e-6 <= t < p.end), None)
        if ph is None or not ph.changes:
            continue
        root = ph.changes[0][1][0]
        for tc, (r, _m) in ph.changes:
            if tc <= t + 1e-6:
                root = r
        nxt = kicks[i + 1][0] if i + 1 < len(kicks) else t + 2.2
        out.append((t, min(2.2, nxt - t), 24 + root % 12, v))
    return out


def drum_score(phrases, bed_sections=(), seed=None, holes=()):
    rng = random.Random(config.cfg()["seeds"]["drums"] if seed is None else seed)
    hits = []
    for i, ph in enumerate(phrases):
        if any(s is ph.section for s in bed_sections):
            continue
        kit = ph.theme.style.get("kit", "cassette")
        hits += [
            (t, f"{kit}:{part}", v)
            for t, part, v in phrase_hits(ph, phrases[i + 1] if i + 1 < len(phrases) else None, rng)
        ]
    # в дырке (там играет живая музыка) ни удара; саб обрезается её краем
    hits = sorted(h for h in hits if not any(lo - 0.05 <= h[0] < hi for lo, hi in holes))
    subs = []
    for t, d, m, v in sub_notes(hits, phrases):
        edge = min([lo for lo, _ in holes if lo > t] + [t + d])
        subs.append((t, min(d, edge - t), m, v))
    return hits, subs


def bed_sections(sections, bed_key: tuple[int, bool] | None):
    """Под готовый бит — виды той же тональности (лад тоже)."""
    if bed_key is None:
        return []
    return [s for s in sections if s.kind == "view" and s.theme is not None and (s.theme.key, s.theme.minor) == bed_key]


def bed_info(path: Path) -> dict:
    """bpm и тональность флипа — из его json рядом (render.bpm/key)."""
    side = path.with_suffix(".json")
    if not side.exists():
        return {}
    d = json.loads(side.read_text())
    r = d.get("render") or {}
    return {
        "bpm": r.get("bpm") or (d.get("chop") or {}).get("grid_bpm"),
        "key": r.get("key") or (d.get("source") or {}).get("key"),
    }


# ---------- звук ----------
# Стемы пишутся КАК ЕСТЬ, float32, без нормализации пика: громкость задаёт
# только gain_db в миксе (одна ступень усиления на все партии).


def _load(path: Path, rate: float = 1.0):
    import numpy as np

    raw = subprocess.run(
        [config.ffmpeg(), "-v", "error", "-i", str(path), "-ac", "1", "-ar", str(round(SR / rate)), "-f", "f32le", "-"],
        capture_output=True,
        check=True,
    ).stdout
    return np.frombuffer(raw, dtype=np.float32).copy()


def write_wav(path: Path, buf) -> None:
    import numpy as np

    data = np.asarray(buf, dtype=np.float32)
    subprocess.run(
        [
            config.ffmpeg(),
            "-y",
            "-v",
            "error",
            "-f",
            "f32le",
            "-ar",
            str(SR),
            "-ac",
            "1",
            "-i",
            "-",
            "-ac",
            "2",
            "-c:a",
            "pcm_f32le",
            str(path),
        ],
        input=data.tobytes(),
        check=True,
    )


def _place(buf, x, t, gain):
    i = int(t * SR)
    n = min(len(x), len(buf) - i)
    if n > 0 and i >= 0:
        buf[i : i + n] += x[:n] * gain


def tom_rate(t: float) -> float:
    return [1.0, 1.12, 1.26][int(t * 8) % 3]  # три высоты тома по ходу филла


def render_drums(out: Path, hits, length: float) -> Path:
    import numpy as np

    buf = np.zeros(int((length + 4) * SR), dtype=np.float32)
    cache = {}
    for t, name, v in hits:
        kit, part = name.split(":")
        rate = tom_rate(t) if part == "tom" else 1.0
        key = (kit, part, rate)
        if key not in cache:
            cache[key] = _load(config.samples_dir() / kits()[kit][part], rate)
        _place(buf, cache[key], t, (v / 127) ** 1.6)
    path = out / "stem_drums.wav"
    write_wav(path, buf)
    return path


def render_sub(out: Path, notes, length: float) -> Path:
    import numpy as np

    buf = np.zeros(int((length + 4) * SR), dtype=np.float32)
    files, cache = sub_files(), {}
    for t, dur, midi, v in notes:
        src = min(files, key=lambda m, midi=midi: abs(m - midi))
        rate = 2 ** ((midi - src) / 12)
        if (src, rate) not in cache:
            cache[(src, rate)] = _load(files[src], rate)
        x = cache[(src, rate)][: int(dur * SR)].copy()
        fade = min(len(x), int(0.04 * SR))
        if fade:
            x[-fade:] *= np.linspace(1, 0, fade, dtype=np.float32)
        _place(buf, x, t, (v / 127) ** (1.2 if v > 50 else 2.5))  # под речью саб едва слышен
    path = out / "stem_sub808.wav"
    write_wav(path, buf)
    return path


def has_rubberband(ff: str) -> bool:
    r = subprocess.run([ff, "-hide_banner", "-filters"], capture_output=True, text=True, check=False)
    return " rubberband " in r.stdout


def render_bed(out: Path, bed: Path, sections, bed_bpm: float, length: float) -> Path:
    """Бит лупом под каждую секцию: темп — ближайшее к биту кратное темпа секции."""
    import numpy as np

    buf = np.zeros(int((length + 4) * SR), dtype=np.float32)
    ff = config.ffmpeg(full=True)
    for s in sections:
        target = min((s.bpm * m for m in (0.5, 1, 2)), key=lambda x: abs(x - bed_bpm))
        dur = s.end - s.start
        raw = subprocess.run(
            [
                ff,
                "-v",
                "error",
                "-stream_loop",
                "-1",
                "-i",
                str(bed),
                "-af",
                (
                    f"rubberband=tempo={target / bed_bpm:.4f},afade=t=in:d=0.4,"
                    f"afade=t=out:st={max(0.0, dur - 0.8):.2f}:d=0.8"
                ),
                "-t",
                f"{dur:.3f}",
                "-ac",
                "1",
                "-ar",
                str(SR),
                "-f",
                "f32le",
                "-",
            ],
            capture_output=True,
            check=True,
        ).stdout
        _place(buf, np.frombuffer(raw, dtype=np.float32), s.start, 1.0)
    path = out / "stem_bed.wav"
    write_wav(path, buf)
    return path
