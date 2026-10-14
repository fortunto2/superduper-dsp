"""Form + theme → Score: sections get themes and tempi, phrases get textures, notes get written.

Film forms (card / talk / view) go through `music.make_phrases`, the layout
the film score has always had. Song forms (intro / verse / chorus / bridge /
outro) are laid out here by energy:

| section | textures (rotated per repeat) | energy | drums |
|---|---|---|---|
| intro | halo / drone + a quiet thread of the motif | low | none |
| verse | ostinato, arp, sequence, retrograde | middle | light |
| chorus | the motif, then inversion / sequence / augmentation | full, rising | full, crash in, fill before |
| bridge | inversion, retrograde — in the subdominant, entered through its dominant | middle | half-time |
| outro | augmentation, halo, drone in 4-bar phrases | falling | soft kick |
"""

import dataclasses
import random

from . import config
from .form import Form
from .music import (
    Phrase,
    Score,
    Section,
    Theme,
    Writer,
    assign_tempo,
    enforce,
    make_phrases,
    modulate,
    snap_changes,
    write_phrase,
)

SONG = {
    "intro": {"pool": ["halo", "drone"], "energy": 0.62, "drums": "none", "bars": 8, "layers": ["thread"]},
    "verse": {"pool": ["ostinato", "arp", "motif_seq", "motif_retro"], "energy": 0.85, "drums": "light", "bars": 8},
    "chorus": {
        "head": "motif",
        "pool": ["motif_inv", "motif_seq", "motif_aug"],
        "energy": 1.02,
        "drums": "full",
        "bars": 8,
    },
    "bridge": {"pool": ["motif_inv", "motif_retro"], "energy": 0.9, "drums": "half", "bars": 8},
    "outro": {"pool": ["motif_aug", "halo", "drone"], "energy": 0.72, "drums": "soft", "bars": 4},
}
BRIDGE_SHIFT = 5  # the bridge sits in the subdominant


def transposed(theme: Theme, shift: int, name: str) -> Theme:
    return dataclasses.replace(
        theme, key=(theme.key + shift) % 12, name=name, chords=[((r + shift) % 12, m) for r, m in theme.chords]
    )


def song_phrases(sections: list[Section]) -> list[Phrase]:
    out: list[Phrase] = []
    occ: dict[str, int] = {}
    used: dict[str, int] = {}
    for s in sections:
        spec = SONG[s.kind]
        n_occ = occ.get(s.kind, 0)
        occ[s.kind] = n_occ + 1
        beat = 60.0 / s.bpm
        bar = beat * 4
        bars = max(1, round(s.length / bar))
        sizes, left = [], bars
        while left > 0:
            size = spec["bars"] if left >= spec["bars"] + 4 or left == spec["bars"] else left
            sizes.append(size)
            left -= size
        t = s.start
        for k, size in enumerate(sizes):
            pool = spec["pool"]
            if k == 0 and "head" in spec:
                tex = spec["head"]
            else:
                tex = pool[(n_occ * 2 + k) % len(pool)]
                if out and out[-1].texture == tex:
                    tex = pool[(n_occ * 2 + k + 1) % len(pool)]
            var = used.get(tex, 0)
            used[tex] = var + 1
            end = s.end if k == len(sizes) - 1 else t + size * bar
            energy = spec["energy"]
            if s.kind == "chorus":
                energy += 0.06 * n_occ
            if s.kind == "outro" and len(sizes) > 1:
                energy *= 1.0 - 0.45 * k / (len(sizes) - 1)
            ph = Phrase(t, end, beat, tex, var, s, energy, drums=spec["drums"])
            if tex in ("halo", "drone", "bed"):
                ph.layers += spec.get("layers", [])
            hr = 2 if tex in ("halo", "drone", "motif_aug") or var % 3 == 2 else 1
            tc = t  # аккорды с начала прогрессии в каждой секции
            ci = sum(len(p.changes) for p in out if p.section is s)
            while tc < end - beat * 0.5:
                ph.changes.append((tc, s.theme.chords[ci % len(s.theme.chords)]))
                ci += 1
                tc += hr * bar
            out.append(ph)
            t = end
    modulate(out)
    return out


def build_score(form: Form, theme: Theme, theme2: Theme | None = None, seed: int | None = None) -> Score:
    if not form.sections:
        raise ValueError("в форме нет секций")
    seed = config.cfg()["seeds"]["notes"] if seed is None else seed
    sections = [Section(fs.start, fs.end, fs.kind, fs.name) for fs in form.sections]
    for fs, s in zip(form.sections, sections):
        hit = theme2 is not None and (
            fs.theme == 2 or any(fs.kind == w or fs.name.startswith(w) for w in form.theme2_for)
        )
        s.theme = theme2 if hit else theme
    assign_tempo(sections, form.tempo_map)
    for fs, s in zip(form.sections, sections):
        if fs.bpm:
            s.bpm = fs.bpm
    if form.is_song:
        for s in sections:
            if s.kind == "bridge":
                base = s.theme
                s.theme = transposed(base, BRIDGE_SHIFT, f"{base.name} · бридж")
        phrases = song_phrases(sections)
        for ph in phrases:
            snap_changes(ph, form.cuts)
    else:
        phrases = make_phrases(sections, form.cuts)
    w = Writer(seed, form.speech)
    w.rng = random.Random(seed)
    for ph in phrases:
        write_phrase(w, ph)
    lifts = [(r, ph.start, ph.end, db) for ph in phrases for r, db in ph.lift.items()]
    for t in form.accents:
        ph = next((p for p in phrases if p.start <= t < p.end), None)
        if ph is None:
            continue
        wide = dataclasses.replace(ph, end=max(ph.end, t + 30.0))
        end = w.motif_line(wide, ph.theme.motif, t, 88, role="accent")
        lifts.append(("pad", t, end, 4.0))
        ph.note = (ph.note + "; " if ph.note else "") + f"акцент {t:.1f} с: мотив в полный голос"
    notes = enforce(w.notes, form.speech, list(form.holes))
    length = max(form.length, sections[-1].end)
    return Score(length, sections, phrases, notes, list(form.speech), list(form.holes), form.source or "форма", lifts)
