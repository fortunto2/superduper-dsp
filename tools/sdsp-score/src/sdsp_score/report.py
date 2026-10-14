"""score.md, score.json and the demo mp3."""

import json
import subprocess
from pathlib import Path

from . import config
from .music import Score, track_of


def fmt_t(t: float) -> str:
    return f"{int(t // 60)}:{t % 60:04.1f}"


def score_json(score: Score) -> dict:
    """What `check` needs to read a render without rebuilding the score."""
    return {
        "length": score.length,
        "sections": [
            {
                "start": s.start,
                "end": s.end,
                "kind": s.kind,
                "name": s.chapter,
                "bpm": round(s.bpm, 2),
                "theme": s.theme.label if s.theme else "",
            }
            for s in score.sections
        ],
        "phrases": [
            {
                "start": round(p.start, 4),
                "end": round(p.end, 4),
                "texture": p.texture,
                "var": p.var,
                "kind": p.section.kind,
                "section": p.section.chapter,
                "lead": track_of(p.theme.style, "lead"),
                "intensity": round(p.intensity, 3),
            }
            for p in score.phrases
        ],
        "speech": score.speech,
        "holes": score.holes,
    }


def write_score_json(out: Path, score: Score) -> None:
    (out / "score.json").write_text(json.dumps(score_json(score), ensure_ascii=False, indent=1) + "\n")


# ---------- demo ----------


def song_demo(score: Score, span=20.0) -> tuple[list[tuple[float, float]], str] | None:
    """Three 20 s windows from three different sections: verse, chorus, bridge
    (the second chorus when there is one), falling back to whatever exists."""
    picks, used = [], set()
    first = {}
    for s in score.sections:
        first.setdefault(s.kind, []).append(s)
    for kind, nth in (("verse", 0), ("chorus", 1), ("bridge", 0), ("chorus", 0), ("outro", 0), ("intro", 0)):
        secs = first.get(kind, [])
        if not secs or len(picks) == 3 or kind in used:
            continue
        s = secs[min(nth, len(secs) - 1)]
        a = max(0.0, min(s.start - 1.0, score.length - span))
        picks.append((a, a + span, s.chapter))
        used.add(kind)
    if not picks:
        return None
    picks.sort()
    return [(a, b) for a, b, _ in picks], " + ".join(f"{n} {fmt_t(a)}" for a, _b, n in picks)


def film_demo(score: Score, span=40.0):
    """Окно, где подряд звучат три разные фактуры, и речи меньше всего."""
    best = None
    for ph in score.phrases:
        t0 = max(0.0, min(ph.start - 1.0, score.length - span))
        t1 = t0 + span
        tex: list[str] = []
        for p in score.phrases:
            if min(p.end, t1) - max(p.start, t0) >= 6.0 and p.texture != "rest" and (not tex or tex[-1] != p.texture):
                tex.append(p.texture)
        talk = sum(max(0.0, min(b, t1) - max(a, t0)) for a, b in score.speech)
        key = (min(len(set(tex)), 3), -talk, len(set(tex)))
        if best is None or key > best[0]:
            best = (key, t0, " → ".join(tex))
    return ([(best[1], best[1] + span)], best[2]) if best else None


def demo_name(demo) -> str:
    return f"demo{round(sum(b - a for a, b in demo[0]))}.mp3"


def write_demo(out: Path, demo, xf=1.5) -> None:
    wins = demo[0]
    cmd = [config.ffmpeg(), "-y", "-v", "error"]
    for a, b in wins:
        cmd += ["-ss", f"{a:.2f}", "-t", f"{b - a:.2f}", "-i", str(out / "score-dry.wav")]
    chainf, last = [], "[0:a]"
    for i in range(1, len(wins)):
        chainf.append(f"{last}[{i}:a]acrossfade=d={xf}[x{i}]")
        last = f"[x{i}]"
    total = sum(b - a for a, b in wins) - xf * (len(wins) - 1)
    chainf.append(f"{last}loudnorm=I=-16:TP=-1.5:LRA=11,afade=t=in:d=0.5,afade=t=out:st={total - 1.5:.2f}:d=1.5[o]")
    subprocess.run(
        cmd
        + [
            "-filter_complex",
            ";".join(chainf),
            "-map",
            "[o]",
            "-ar",
            "48000",
            "-c:a",
            "libmp3lame",
            "-b:a",
            "192k",
            str(out / demo_name(demo)),
        ],
        check=True,
    )


# ---------- score.md ----------


def form_md(score: Score, div: dict, motif_src: dict, demo=None, critique=(), cmd="", levels=None, tracks=()) -> str:
    levels = levels or {}
    themes = []
    for ph in score.phrases:
        if ph.theme not in themes:
            themes.append(ph.theme)
    lines = ["# Форма партитуры", ""] + ([f"`{cmd}`", ""] if cmd else [])
    for th in themes:
        d = th.to_dict()
        motif = " ".join(f"{x}:{b:g}" for x, b in th.motif)
        st = th.style
        lines.append(
            f"- **{th.label}**, {th.bpm:g} bpm · аккорды {' '.join(d['chords'])} · "
            f"мотив (ступень:доли) {motif} — {motif_src.get(th.name, 'из темы')} · "
            f"ведёт {st['lead'][0]}#{st['lead'][1]}, остинато {st['ostinato'][0]}"
            f"#{st['ostinato'][1]}, арпеджио {st['arp'][0]}#{st['arp'][1]}"
        )
    lines += [
        "",
        f"Форма: {score.speech_src} · длина {fmt_t(score.length)} · речь {len(score.speech)} кусков",
        "",
        (
            f"**Разнообразие:** 8-тактовых окон {div['windows']}, повторяют предыдущее "
            f"{div['repeat_share']:.0%}, любое прежнее {div['any_repeat_share']:.0%}; "
            f"смен фактуры {div['texture_changes']} на {len(score.phrases)} фраз; "
            f"фактур {len(div['textures'])} ({', '.join(div['textures'])}); "
            f"темпы {', '.join(str(t) for t in div['tempi'])} bpm; "
            f"пустот > 20 с по нотам: {len(div['gaps'])}"
        ),
        "",
    ]
    if tracks:
        lines += ["**Партии** (score.mid — трек на партию, ударные канал 10): " + ", ".join(tracks), ""]
    if critique:
        lines += ["**Проверка** (`sdsp-score check`):", ""] + [f"- {c}" for c in critique] + [""]
    if demo:
        lines += [f"**{demo_name(demo)}:** {demo[1]}", ""]
    lines += ["| # | время | секция | тема | bpm | LUFS | фактура·вариант |", "|---|---|---|---|---|---|---|"]
    for i, s in enumerate(score.sections, 1):
        tex = ", ".join(
            f"{p.label}·{p.var}" + (f" ({p.note})" if p.note else "") for p in score.phrases if p.section is s
        )
        lvl = f"{levels[i - 1]:.1f}" if i - 1 < len(levels) and levels[i - 1] is not None else "—"
        lines.append(
            f"| {i} | {fmt_t(s.start)}–{fmt_t(s.end)} | {s.kind} {s.chapter} "
            f"| {s.theme.label if s.theme else ''} | {s.bpm:.0f} | {lvl} | {tex} |"
        )
    return "\n".join(lines) + "\n"
