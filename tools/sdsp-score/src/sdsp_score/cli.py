"""sdsp-score — a theme becomes an arranged piece on SuperDuper DSP instruments.

    sdsp-score theme.json --form "intro:8,verse:16,chorus:16,bridge:8,chorus:16,outro:8" --out out/
    sdsp-score theme.json --length 180 --out out/ --fit
    sdsp-score theme.json,theme2.json film.form.json --out out/      # a film form from video-analyzer
    sdsp-score theme.json --length 90 --out out/ --midi-only         # score.mid + toml, no audio
    sdsp-score check out/                                            # read a render, change nothing

Writes form.json, score.json, score.mid, stem_*.toml, score.md; with
rendering also stem_*.wav, score.wav, score-dry.wav, mix.json, check.json and
demo<N>.mp3. Refusals are exit 2 with the reason, before any audio is made:
a theme without chords or motif, a tempo outside 40..200, an unknown form
section, a missing sdsp-chain / ffmpeg / drum sample / unknown kit, a bed
without key or tempo, or an ffmpeg without rubberband for it.
Exit 1: the render has a silent stretch longer than 20 s.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

from . import check as ck
from . import config, midi, report
from . import drums as td
from . import form as fm
from .arrange import build_score
from .fit import fit
from .mix import RenderError, initial_spec, render_mix, render_stems, write_stem_tomls
from .music import Theme, diversity, motif_fit, motif_from_midi, parse_chord


class Refusal(Exception):
    pass


def load_theme(path: str) -> tuple[Theme, str]:
    p = Path(os.path.expanduser(path))
    try:
        d = json.loads(p.read_text())
    except (OSError, json.JSONDecodeError) as bad:
        raise Refusal(f"тема {p}: {bad}") from bad
    try:
        theme = Theme.from_dict(d)
    except ValueError as bad:
        raise Refusal(f"тема {p}: неизвестный аккорд или тональность «{bad}»") from bad
    src = "из темы"

    def near(x):
        x = os.path.expanduser(x)
        return x if os.path.isabs(x) else str(p.parent / x)

    if d.get("motif_midi") and not theme.motif:
        midi_path = near(d["motif_midi"])
        if not os.path.exists(midi_path):
            raise Refusal(f"тема {p}: нет motif_midi {midi_path}")
        how: dict = {}
        theme.motif = motif_from_midi(midi_path, theme.key, theme.minor, how)
        src = f"из {os.path.basename(midi_path)} ({how.get('how', '?')})"
    if d.get("motif_ref") and os.path.exists(near(d["motif_ref"])):
        fit_r = motif_fit(theme.motif, theme, json.loads(Path(near(d["motif_ref"])).read_text()))
        src += f"; совпадение с {os.path.basename(d['motif_ref'])} r={fit_r:.2f}"
    if not theme.chords:
        raise Refusal(f"тема {p}: нет аккордов")
    if not theme.motif:
        raise Refusal(f"тема {p}: нет мотива (motif или motif_midi)")
    try:
        fm.check_bpm(theme.bpm, f"тема {p.name}")
    except fm.FormError as bad:
        raise Refusal(str(bad)) from bad
    return theme, src


def make_form(a, theme: Theme) -> fm.Form:
    factors = theme.style.get("form_tempo")
    try:
        if a.form_json:
            return fm.load(a.form_json)
        if a.length:
            return fm.song_form(fm.spec_for_length(a.length, theme.bpm, factors), theme.bpm, factors)
        return fm.song_form(fm.parse_spec(a.form or fm.DEFAULT_FORM), theme.bpm, factors)
    except fm.FormError as bad:
        raise Refusal(str(bad)) from bad


def load_bed(form: fm.Form):
    if not form.bed:
        return None
    path = Path(os.path.expanduser(form.bed["path"]))
    if not path.exists():
        raise Refusal(f"бит: нет файла {path}")
    info = td.bed_info(path)
    key, bpm = form.bed.get("key") or info.get("key"), form.bed.get("bpm") or info.get("bpm")
    if not key or not bpm:
        raise Refusal(f"бит {path.name}: неизвестны тональность или темп — key/bpm в bed формы")
    ff = config.ffmpeg(full=True)
    if not td.has_rubberband(ff):
        raise Refusal(f"бит: в {ff} нет фильтра rubberband (brew install ffmpeg-full или SDSP_FFMPEG)")
    return {"path": path, "key": parse_chord(key), "bpm": float(bpm)}


def preflight_tools() -> None:
    chain = config.chain_bin()
    if not (os.path.isfile(chain) and os.access(chain, os.X_OK)):
        raise Refusal(f"нет sdsp-chain: {chain} (SDSP_CHAIN, или cargo build --release -p sdsp-chain)")
    ff = config.ffmpeg()
    if not shutil.which(ff) and not os.access(ff, os.X_OK):
        raise Refusal(f"нет ffmpeg: {ff} (SDSP_FFMPEG)")


def parse_args(argv):
    ap = argparse.ArgumentParser(prog="sdsp-score", description=__doc__.split("\n")[0])
    ap.add_argument("themes", help="theme.json или theme.json,theme2.json")
    ap.add_argument("form_json", nargs="?", help="form.json (из video-analyzer или сохранённый)")
    g = ap.add_mutually_exclusive_group()
    g.add_argument("--form", help=f"секции в тактах, по умолчанию «{fm.DEFAULT_FORM}»")
    g.add_argument("--length", type=float, help="секунды: форма по умолчанию, подогнанная по длине")
    ap.add_argument("--out", required=True)
    ap.add_argument("--seed", type=int)
    r = ap.add_mutually_exclusive_group()
    r.add_argument("--render", action="store_true", help="(по умолчанию) стемы, микс, проверка, демо")
    r.add_argument(
        "--midi-only", "--no-render", dest="midi_only", action="store_true", help="только score.mid, toml и форма"
    )
    ap.add_argument("--fit", action="store_true", help="подогнать гейны микса по check")
    a = ap.parse_args(argv)
    if a.form_json and (a.form or a.length):
        ap.error("form.json или --form/--length, не вместе")
    return a


def run(a, cmd: str) -> int:
    paths = [x for x in a.themes.split(",") if x.strip()]
    themes = [load_theme(x) for x in paths[:2]]
    theme, theme2 = themes[0][0], (themes[1][0] if len(themes) > 1 else None)
    motif_src = {t.name: s for t, s in themes}
    form = make_form(a, theme)
    try:
        score = build_score(form, theme, theme2, a.seed)
    except ValueError as bad:
        raise Refusal(f"форма: {bad}") from bad
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    fm.save(form, out / "form.json")
    report.write_score_json(out, score)
    bed = None if a.midi_only else load_bed(form)
    bsecs = td.bed_sections(score.sections, bed["key"]) if bed else []
    hits, subs = td.drum_score(score.phrases, bsecs, holes=score.holes)
    tracks = midi.write(out / "score.mid", score, hits, subs)
    write_stem_tomls(out, score)
    div = diversity(score)
    demo = (
        ([(x, y) for x, y in form.demo], "окна формы")
        if form.demo
        else (report.song_demo(score) if form.is_song else report.film_demo(score))
    )
    print(
        f"{len(score.sections)} секций, {len(score.phrases)} фраз, {report.fmt_t(score.length)}, темпы {div['tempi']}"
    )
    print(
        f"разнообразие: окон {div['windows']}, повтор предыдущего {div['repeat_share']:.0%}, "
        f"любого прежнего {div['any_repeat_share']:.0%}, фактур {len(div['textures'])}"
    )
    print(f"score.mid: {len(tracks)} партий — {', '.join(tracks)}")
    (out / "score.md").write_text(report.form_md(score, div, motif_src, demo, cmd=cmd, tracks=tracks))
    if a.midi_only:
        return 0
    preflight_tools()
    bad = td.preflight(hits, subs)
    if bad:
        raise Refusal(bad)
    key_audio = bool(form.key_audio)
    if key_audio:
        if not os.path.exists(form.key_audio):
            raise Refusal(f"нет key_audio {form.key_audio}")
        subprocess.run(
            [
                config.ffmpeg(),
                "-y",
                "-v",
                "error",
                "-i",
                form.key_audio,
                "-ac",
                "2",
                "-ar",
                str(config.cfg()["sample_rate"]),
                str(out / "key.wav"),
            ],
            check=True,
        )
    names = render_stems(out, score, hits, subs, bed, bsecs)
    spec = initial_spec(score, names)
    render_mix(out, score, spec, key_audio)
    res = ck.check(out)
    notes = ck.lines(res)
    if a.fit:
        before = notes
        res, log = fit(out, score, spec, key_audio, target_lufs=-14.0 if form.is_song else -20.0)
        notes = [f"до fit: {before[0]}"] + log + ck.lines(res)
    for n in notes:
        print("check: " + n)
    if demo:
        report.write_demo(out, demo)
    levels = [s["lufs"] for s in res["sections"]]
    (out / "score.md").write_text(report.form_md(score, div, motif_src, demo, notes, cmd, levels, tracks))
    print(f"→ {out}/score.wav, score.mid" + (f", {report.demo_name(demo)} ({demo[1]})" if demo else ""))
    return 1 if res["gaps"] else 0


def main(argv=None) -> int:
    argv = list(sys.argv[1:] if argv is None else argv)
    if argv[:1] == ["check"]:
        if len(argv) != 2 or not (Path(argv[1]) / "score.json").exists():
            print("UNKNOWN — sdsp-score check <out>: нужна папка рендера со score.json", file=sys.stderr)
            return 2
        for n in ck.lines(ck.check(argv[1])):
            print(n)
        return 0
    a = parse_args(argv)
    try:
        return run(a, "sdsp-score " + " ".join(argv))
    except Refusal as bad:
        print(f"UNKNOWN — {bad}", file=sys.stderr)
        return 2
    except RenderError as bad:
        print(f"UNKNOWN — {bad}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
