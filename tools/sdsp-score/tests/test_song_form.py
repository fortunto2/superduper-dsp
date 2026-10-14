"""A song form without a film: sections by energy, tempo per section, no repeats."""

import pytest

from sdsp_score import drums as td
from sdsp_score import form as fm
from sdsp_score.arrange import build_score
from sdsp_score.music import Theme, diversity, role_of

DM = Theme.from_dict(
    {
        "name": "dm",
        "key": "D",
        "mode": "minor",
        "bpm": 72,
        "chords": ["Dm", "Dm", "A#", "Dm", "Dm", "A#", "A#", "Dm"],
        "motif": [
            {"deg": 1, "beats": 1},
            {"deg": 3, "beats": 0.5},
            {"deg": 4, "beats": 0.5},
            {"deg": 5, "beats": 1},
            {"deg": 4, "beats": 0.5},
            {"deg": 3, "beats": 0.5},
        ],
        "style": {"kit": "kubyz", "under_motif": "ostinato"},
    }
)
AM = Theme.from_dict(
    {
        "name": "am",
        "key": "A",
        "mode": "minor",
        "bpm": 86,
        "chords": ["Am", "Am", "Em", "Am", "C", "G", "Em", "Am"],
        "motif": [
            {"deg": 1},
            {"deg": 1},
            {"deg": 0},
            {"deg": 1, "beats": 0.5},
            {"deg": 1, "beats": 0.5},
            {"deg": 2},
            {"deg": 1, "beats": 1.5},
            {"deg": 0, "beats": 1.5},
        ],
        "style": {"lead": ["kubyz", 1], "under_motif": "arp"},
    }
)
G = Theme.from_dict(
    {
        "name": "g",
        "key": "G",
        "mode": "major",
        "bpm": 120,
        "chords": ["G", "G", "Em", "G", "G", "Am", "Em", "G"],
        "motif": [
            {"deg": 1, "beats": 2},
            {"deg": 4, "beats": 0.5},
            {"deg": 2, "beats": 1.5},
            {"deg": 1},
            {"deg": 3},
            {"deg": 1, "beats": 0.5},
            {"deg": 1},
        ],
        "style": {"kit": "909", "feel": "straight"},
    }
)


def song(theme=DM, spec=fm.DEFAULT_FORM):
    form = fm.song_form(fm.parse_spec(spec), theme.bpm)
    return form, build_score(form, theme)


def test_the_form_parses_into_bars_and_seconds():
    sp = fm.parse_spec("intro:8, verse:16,chorus:16,outro:4")
    assert sp == [("intro", 8), ("verse", 16), ("chorus", 16), ("outro", 4)]
    form = fm.song_form(sp, 72)
    assert [s.name for s in form.sections] == ["intro 1", "verse 1", "chorus 1", "outro 1"]
    for s in form.sections:  # длина секции — её такты в её темпе
        assert abs((s.end - s.start) - s.bars * 4 * 60 / s.bpm) < 1e-3
    assert all(a.end == b.start for a, b in zip(form.sections, form.sections[1:]))


@pytest.mark.parametrize("bad", ["intro:8,solo:8", "verse:6", "verse", "verse:0", ""])
def test_a_bad_form_is_refused(bad):
    with pytest.raises(fm.FormError):
        fm.parse_spec(bad)


def test_tempo_follows_the_section():
    form, score = song()
    bpm = {s.kind: s.bpm for s in score.sections}
    assert bpm["chorus"] > bpm["verse"] > bpm["bridge"] and bpm["outro"] < bpm["verse"]
    for ph in score.phrases:
        assert abs(60.0 / ph.beat - ph.section.bpm) < 1e-6


def test_a_section_tempo_outside_40_200_is_refused():
    with pytest.raises(fm.FormError, match="вне 40..200"):
        fm.song_form(fm.parse_spec("chorus:8"), 195)  # припев ×1.05 = 204.75


def test_sections_get_their_own_texture_and_energy():
    _, score = song()
    by = {}
    for ph in score.phrases:
        by.setdefault(ph.section.kind, []).append(ph)
    assert {p.texture for p in by["intro"]} <= {"halo", "drone"}
    assert all("thread" in p.layers for p in by["intro"])
    chorus_heads = [p for p in by["chorus"] if p.start == p.section.start]
    assert chorus_heads and all(p.texture == "motif" for p in chorus_heads)
    assert {p.texture for p in by["verse"]} & {"ostinato", "arp"}
    assert {p.texture for p in by["bridge"]} <= {"motif_inv", "motif_retro"}
    mean = {k: sum(p.intensity for p in v) / len(v) for k, v in by.items()}
    assert mean["intro"] < mean["verse"] < mean["chorus"]
    outro = by["outro"]
    assert all(a.intensity > b.intensity for a, b in zip(outro, outro[1:]))


def test_choruses_rise_and_the_bridge_modulates_through_a_dominant():
    _, score = song()
    choruses = [p for p in score.phrases if p.section.kind == "chorus" and p.start == p.section.start]
    assert [round(p.intensity, 2) for p in choruses] == sorted(round(p.intensity, 2) for p in choruses)
    i = next(i for i, p in enumerate(score.phrases) if p.section.kind == "bridge")
    bridge = score.phrases[i]
    assert bridge.theme.key == (DM.key + 5) % 12  # G минор — субдоминанта
    assert score.phrases[i - 1].changes[-1][1] == ((bridge.theme.key + 7) % 12, False)
    back = next(p for p in score.phrases[i:] if p.section.kind == "chorus")
    j = score.phrases.index(back)
    assert score.phrases[j - 1].changes[-1][1] == ((DM.key + 7) % 12, False)  # A — домой


def test_drums_follow_the_energy():
    _, score = song()
    hits, subs = td.drum_score(score.phrases)

    def parts(kind):
        secs = [s for s in score.sections if s.kind == kind]
        return [h[1].split(":")[1] for h in hits if any(s.start <= h[0] < s.end for s in secs)]

    assert parts("intro") == []
    assert set(parts("outro")) <= {"soft"}
    assert {"kick", "hat", "crash"} <= set(parts("chorus"))
    verse, chorus = parts("verse"), parts("chorus")
    assert verse.count("kick") / max(1, verse.count("hat")) <= chorus.count("kick") / chorus.count("hat") + 1
    first_chorus = next(s for s in score.sections if s.kind == "chorus")
    toms = [t for t, p, _ in hits if p.endswith(":tom") and first_chorus.start - 3 < t < first_chorus.start]
    assert len(toms) >= 4  # филл в припев


@pytest.mark.parametrize("theme", [DM, AM, G], ids=["dm72", "am86", "g120"])
@pytest.mark.parametrize(
    "spec", [fm.DEFAULT_FORM, "intro:4,verse:8,chorus:8,outro:4", "intro:8,chorus:32,bridge:16,chorus:16,outro:8"]
)
def test_no_eight_bar_window_repeats_the_previous(theme, spec):
    _, score = song(theme, spec)
    d = diversity(score)
    assert d["windows"] >= 2
    assert d["repeat_share"] == 0.0, d
    assert d["gaps"] == []


@pytest.mark.parametrize("seconds", [30, 60, 180, 400])
def test_length_gives_a_form_of_that_length(seconds):
    sp = fm.spec_for_length(seconds, 72)
    form = fm.song_form(sp, 72)
    kinds = [k for k, _ in sp]
    assert kinds[0] == "intro" and kinds[-1] == "outro" and "chorus" in kinds
    block = form.length / sum(b for _, b in sp) * 4
    assert abs(form.length - seconds) <= block, (sp, form.length)


def test_length_too_short_is_refused():
    with pytest.raises(fm.FormError):
        fm.spec_for_length(10, 72)


def test_lead_is_heard_in_the_chorus_and_silent_under_nothing():
    _, score = song()
    lead = [n for t, ns in score.notes.items() if role_of(t) == "lead" for n in ns]
    for s in (s for s in score.sections if s.kind == "chorus"):
        assert any(s.start <= n[0] < s.end for n in lead)


def test_form_json_round_trip(tmp_path):
    form, _ = song()
    fm.save(form, tmp_path / "f.json")
    back = fm.load(tmp_path / "f.json")
    assert [(s.kind, s.bars, s.bpm) for s in back.sections] == [(s.kind, s.bars, s.bpm) for s in form.sections]
    assert back.is_song
