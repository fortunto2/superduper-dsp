"""The film score's behaviour, now driven by form.json instead of an OTIO plan.

Ported from video-analyzer's test_theme_score.py (31 tests). What was about
reading the plan and its subtitles stayed there with the OTIO→form adapter;
what is about the music is here, on the same timings.
"""

import itertools

import pytest

from sdsp_score import drums as td
from sdsp_score.arrange import build_score
from sdsp_score.form import Form, FormSection
from sdsp_score.mix import automation, gain_of, zone_value, zones_of
from sdsp_score.music import MELODIC, Score, Theme, diversity, motif_fit, role_of

THEME = Theme.from_dict(
    {
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
    }
)
SPEECH = [(10.0, 40.0), (50.0, 70.0)]
CUTS = [3.0, 83.0, 86.0, 106.0, 123.3]


def film_form(speech=SPEECH, holes=(), accents=(), tempo_map=(), theme2_for=()):
    """Two chapters with cards; the second chapter's view is cut into three shots.
    Sections as the video-analyzer adapter splits them by speech."""
    secs = [FormSection(0, 3, "card", "01-morning")]
    t = 3.0
    for a, b in speech:
        if a > t:
            secs.append(FormSection(t, a, "view", "01-morning"))
        secs.append(FormSection(a, b, "talk", "01-morning"))
        t = b
    secs.append(FormSection(t, 83, "view", "01-morning"))
    secs += [FormSection(83, 86, "card", "02-view"), FormSection(86, 146.3, "view", "02-view")]
    return Form(
        146.3,
        secs,
        CUTS,
        list(speech),
        list(holes),
        list(accents),
        theme2_for=list(theme2_for),
        tempo_map=list(tempo_map),
    )


def test_the_form_is_not_one_texture():
    d = diversity(build_score(film_form(), THEME))
    assert len(d["textures"]) >= 4, d
    assert d["texture_changes"] >= 5, d
    assert d["repeat_share"] < 0.2, d


def test_no_two_identical_eight_bar_windows_in_a_row():
    assert diversity(build_score(film_form(speech=[]), THEME))["repeat_share"] == 0.0


def test_auto_tempo_is_faster_for_views_slower_for_talk():
    score = build_score(film_form(), THEME)
    bpm = {s.kind: s.bpm for s in score.sections}
    assert bpm["view"] > 72 > bpm["talk"]
    assert all(72 * 0.85 - 1e-6 <= s.bpm <= 72 * 1.15 + 1e-6 for s in score.sections)


def test_tempo_map_is_applied():
    score = build_score(film_form(tempo_map=[(0, 60, 60), (60, 300, 84)]), THEME)
    for sec in score.sections:
        want = 60.0 if (sec.start + sec.end) / 2 < 60 else 84.0
        assert abs(sec.bpm - want) < 1e-6, (sec.start, sec.bpm)
    for ph in score.phrases:
        assert abs(60.0 / ph.beat - ph.section.bpm) < 1e-6


def test_a_zero_tempo_in_the_map_is_refused_not_a_division_by_zero():
    with pytest.raises(ValueError, match="вне 40..200"):
        build_score(film_form(tempo_map=[(0, 300, 0)]), THEME)


def test_under_speech_only_a_quiet_bass_or_pad():
    score = build_score(film_form(), THEME)
    for track, notes in score.notes.items():
        for at, dur, _pitch, vel in notes:
            if any(lo < at + dur and at < hi for lo, hi in SPEECH):
                assert role_of(track) not in MELODIC, (track, at)
                assert vel <= 52, (track, at, vel)


def test_silence_is_a_hole():
    score = build_score(film_form(speech=[], holes=[(100.0, 130.0)]), THEME)
    for notes in score.notes.values():
        for at, dur, _p, _v in notes:
            assert at + dur <= 100.0 + 1e-6 or at >= 130.0, at


def test_a_chord_changes_on_the_beat_nearest_a_cut():
    score = build_score(film_form(speech=[]), THEME)
    cut = 106.0
    ph = next(p for p in score.phrases if p.start < cut < p.end)
    beat_at_cut = ph.start + round((cut - ph.start) / ph.beat) * ph.beat
    assert any(abs(t - beat_at_cut) < 1e-6 for t, _ in ph.changes), ph.changes


def test_no_silent_stretch_longer_than_twenty_seconds():
    assert diversity(build_score(film_form(), THEME))["gaps"] == []


def test_the_variant_is_a_parameter_not_code():
    d = THEME.to_dict()
    d["style"] = {"lead": ["kubyz", 1], "arp": ["pad", 4]}
    score = build_score(film_form(), Theme.from_dict(d))
    leads = [t for t in score.notes if role_of(t) == "lead" and score.notes[t]]
    assert leads == ["lead-kubyz1"]
    assert all(50 <= p <= 74 for _, _, p, _ in score.notes["lead-kubyz1"])


def test_a_second_theme_arrives_through_its_dominant():
    d = THEME.to_dict()
    d.update(key="A", bpm=86, chords=["Am", "Em", "G", "Am"], name="b")
    second = Theme.from_dict(d)
    score = build_score(film_form(theme2_for=["02"]), THEME, second)
    switch = next(i for i, p in enumerate(score.phrases) if p.theme is second)
    assert switch > 0
    assert score.phrases[switch - 1].changes[-1][1] == (4, False)  # E — доминанта ля минора
    assert score.phrases[switch].changes[0][1] == (9, True)
    assert all(s.bpm >= 86 * 0.85 for s in score.sections if s.theme is second)


def test_an_empty_form_is_a_refusal():
    with pytest.raises(ValueError):
        build_score(Form(0, []), THEME)


# ---------- long speech ----------


def long_talk(accents=()):
    # the adapter folds the 5 s view before the speech into it (sections under 6 s merge)
    return Form(
        120,
        [FormSection(0, 75, "talk", "01"), FormSection(75, 120, "view", "01")],
        [],
        [(5.0, 75.0)],
        accents=list(accents),
    )


def test_under_long_speech_the_theme_keeps_a_quiet_thread():
    score = build_score(long_talk(), THEME)
    thread = [n for t, ns in score.notes.items() if role_of(t) == "thread" for n in ns]
    inside = [n for n in thread if 5.0 < n[0] < 75.0]
    assert len(inside) >= 4, thread
    assert all(v <= 52 for *_, v in inside)


def test_after_a_minute_of_speech_the_motif_comes_in_full_voice():
    score = build_score(long_talk(), THEME)
    first = next(p for p in score.phrases if p.start >= 75.0 - 1e-6)
    assert first.texture == "motif"
    assert ("pad", first.start, first.end, 4.0) in score.lifts


def test_an_accent_plays_the_motif_over_speech():
    score = build_score(long_talk(accents=[30.0]), THEME)
    acc = [n for t, ns in score.notes.items() if role_of(t) == "accent" for n in ns]
    assert len(acc) == len(THEME.motif) and acc[0][0] == 30.0
    assert min(v for *_, v in acc) > 60


def test_a_pause_of_one_and_a_half_seconds_is_honest():
    form = Form(
        120, [FormSection(0, 20, "talk", "01"), FormSection(20, 120, "view", "01")], [], [(0.0, 10.0), (11.6, 20.0)]
    )
    score = build_score(form, THEME)
    fill = [n for t, ns in score.notes.items() if role_of(t) == "lead" for n in ns if 10.0 <= n[0] < 11.6]
    assert fill, "в паузе 1.6 с лид должен ответить"
    pts = automation(score, "pad-pad9", 6.0)

    def at(t):
        for (t0, v0), (t1, v1) in itertools.pairwise(pts):
            if t0 <= t <= t1:
                return v0 + (v1 - v0) * (t - t0) / max(1e-9, t1 - t0)
        return pts[-1][1]

    assert at(5.0) == -6.0 and at(15.0) == -6.0
    assert at(10.8) == 0.0


def test_the_quiet_lead_preset_is_trimmed_from_the_instrument_file():
    score = Score(0, [], [], {}, [], [])
    assert gain_of(score, "lead-wave14") == 11.0
    assert gain_of(score, "lead-kubyz1") == 4.0


def test_the_motif_matches_its_reference_by_pitch_profile():
    th = Theme.from_dict(
        {"key": "A", "mode": "minor", "chords": ["Am"], "motif": [{"deg": 1}, {"deg": 1}, {"deg": 0}, {"deg": 2}]}
    )
    ref = [{"midi": 57, "dur": 2.0}, {"midi": 55, "dur": 0.5}, {"midi": 59, "dur": 0.5}]
    assert motif_fit(th.motif, th, ref) > 0.6
    assert motif_fit([(5, 1.0), (6, 1.0), (3, 1.0)], th, ref) < 0.3


# ---------- rhythm section ----------


def test_under_speech_only_a_rare_soft_kick():
    hits, _ = td.drum_score(build_score(film_form(), THEME).phrases)
    for t, part, v in hits:
        if any(lo <= t < hi for lo, hi in SPEECH):
            assert part.endswith(":soft") and v <= 52, (t, part, v)


def test_views_have_kick_hat_and_backbeat_in_double_time():
    score = build_score(film_form(speech=[]), THEME)
    hits, subs = td.drum_score(score.phrases)
    ph = next(p for p in score.phrases if p.texture == "arp" or p.texture.startswith("motif_s"))
    mine = [(t, part.split(":")[1]) for t, part, _ in hits if ph.start <= t < ph.end]
    parts = {p for _, p in mine}
    assert {"kick", "hat"} <= parts and parts & {"rim", "clap"}
    hats = sorted(t for t, p in mine if p == "hat")
    assert min(b - a for a, b in itertools.pairwise(hats)) < ph.beat * 0.3
    kicks = [t for t, p in mine if p == "kick"]
    assert any(abs(s[0] - kicks[0]) < 1e-6 for s in subs)


def test_the_sub_follows_the_chord_root():
    score = build_score(film_form(speech=[]), THEME)
    _, subs = td.drum_score(score.phrases)
    for t, _d, midi, _v in subs:
        ph = next(p for p in score.phrases if p.start - 1e-6 <= t < p.end)
        root = [c for tc, c in ph.changes if tc <= t + 1e-6][-1][0]
        assert midi == 24 + root % 12


def test_a_fill_leads_into_the_card():
    score = build_score(film_form(speech=[]), THEME)
    hits, _ = td.drum_score(score.phrases)
    card = next(p for i, p in enumerate(score.phrases) if p.texture == "card" and i > 0)
    toms = [t for t, part, _ in hits if part.endswith(":tom") and card.start - 4 < t < card.start]
    assert len(toms) >= 4


def test_a_bed_takes_only_views_in_its_key():
    d = THEME.to_dict()
    d.update(key="A", name="b")
    score = build_score(film_form(theme2_for=["02"]), THEME, Theme.from_dict(d))
    secs = td.bed_sections(score.sections, (2, True))
    assert secs and all(s.kind == "view" and s.theme is THEME for s in secs)
    hits, _ = td.drum_score(score.phrases, secs)
    assert not any(s.start <= t < s.end for t, _, _ in hits for s in secs)


def test_a_hole_is_silent_in_drums_sub_and_the_mix_automation():
    hole = (100.0, 130.0)
    score = build_score(film_form(speech=[], holes=[hole]), THEME)
    hits, subs = td.drum_score(score.phrases, holes=[hole])
    assert not any(hole[0] - 0.05 <= t < hole[1] for t, _, _ in hits)
    assert all(t + d <= hole[0] + 1e-6 or t >= hole[1] for t, d, _, _ in subs)
    pts = dict(automation(score, "pad-pad9", 0.0))
    assert min(v for t, v in pts.items() if hole[0] < t < hole[1]) <= -90.0


# ---------- zones ----------


def test_a_modulation_opens_at_card_level():
    d = THEME.to_dict()
    d.update(key="A", name="b")
    score = build_score(film_form(theme2_for=["02"]), THEME, Theme.from_dict(d))
    mod = next(p for p in score.phrases if p.note.startswith("модуляция"))
    zs = zones_of(score)
    assert (mod.start, min(mod.end, mod.start + 2 * mod.bar), "card") in zs
    assert all(a < b for a, b, _ in zs)
    assert all(zs[i][1] <= zs[i + 1][0] + 1e-9 for i in range(len(zs) - 1))


def test_zone_gain_reaches_every_track_with_a_soft_edge():
    score = build_score(film_form(speech=[]), THEME)
    zones = [(0.0, 50.0, -10.0), (50.0, 200.0, 4.0)]
    assert zone_value(zones, 20.0) == -10.0 and zone_value(zones, 100.0) == 4.0
    assert -10.0 < zone_value(zones, 50.0) < 4.0
    pts = dict(automation(score, "lead-wave14", 0.0, zones=zones))
    assert pts[0.0] == -10.0 and max(pts.values()) == 4.0


def test_a_hole_is_its_own_zone():
    score = build_score(film_form(holes=[(100.0, 130.0)]), THEME)
    zs = zones_of(score)
    assert (100.0, 130.0, "hole") in zs
    assert not any(a < 130.0 and b > 100.0 and k != "hole" for a, b, k in zs)
