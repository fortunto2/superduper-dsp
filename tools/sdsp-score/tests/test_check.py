"""check reads synthetic stems with known levels; fit turns one reading into one correction."""

import json

import numpy as np
import pytest

from sdsp_score import check as ck
from sdsp_score.drums import SR, write_wav
from sdsp_score.fit import adjust
from sdsp_score.mix import MixSpec


def tone(freq, seconds, db, silent=()):
    t = np.arange(int(seconds * SR)) / SR
    x = (10 ** (db / 20)) * np.sin(2 * np.pi * freq * t)
    for a, b in silent:
        x[int(a * SR) : int(b * SR)] = 0.0
    return x.astype(np.float32)


@pytest.fixture
def render(tmp_path):
    """A 40 s 'render': lead 12 dB over the pad in one motif phrase, then 25 s of nothing."""
    out = tmp_path
    lead = tone(880, 40, -20, silent=[(15, 40)])
    pad = tone(660, 40, -32, silent=[(15, 40)])
    write_wav(out / "stem_lead-wave14.wav", lead)
    write_wav(out / "stem_pad-pad9.wav", pad)
    write_wav(out / "score.wav", lead + pad)
    write_wav(out / "score-dry.wav", lead + pad)
    (out / "score.json").write_text(
        json.dumps(
            {
                "length": 40.0,
                "holes": [],
                "speech": [],
                "sections": [
                    {"start": 0, "end": 15, "kind": "chorus", "name": "chorus 1"},
                    {"start": 15, "end": 40, "kind": "outro", "name": "outro 1"},
                ],
                "phrases": [{"start": 0, "end": 15, "texture": "motif", "lead": "lead-wave14", "kind": "chorus"}],
            }
        )
    )
    MixSpec({"lead-wave14": 0.0, "pad-pad9": 0.0}).save(out / "mix.json")
    return out


def test_check_reads_the_level_difference_in_lufs(render):
    res = ck.check(render)
    d = res["lead_vs_acc"]["lead-wave14"]
    # 12 dB of amplitude; K-weighting adds ~+0.3 dB at 880 Hz over 660 Hz
    assert 11.0 < d["median"] < 13.5, d
    assert res["sections"][0]["lufs"] > res["sections"][1]["lufs"] + 30


def test_check_adds_the_mix_gain_of_each_stem(render):
    MixSpec({"lead-wave14": -6.0, "pad-pad9": 0.0}).save(render / "mix.json")
    d = ck.check(render)["lead_vs_acc"]["lead-wave14"]
    assert 5.0 < d["median"] < 7.5, d


def test_check_finds_a_silent_stretch(render):
    assert ck.check(render)["gaps"] and ck.check(render)["gaps"][0][0] >= 14.0


def test_check_only_reads(render):
    before = {p.name: p.stat().st_mtime_ns for p in render.iterdir()}
    ck.check(render)
    after = {p.name: p.stat().st_mtime_ns for p in render.iterdir() if p.name != "check.json"}
    assert after == before


def reading(**over):
    res = {"lead_vs_acc": {}, "vs_lead": {}, "low_vs_mid": -6.0}
    res.update(over)
    return res


def test_fit_lifts_a_drowned_lead_and_leaves_a_good_one():
    spec = MixSpec({"lead-wave14": 0.0, "pad-pad9": 0.0})
    moved = adjust(reading(lead_vs_acc={"lead-wave14": {"median": 0.0, "worst": -2.0, "phrases": 4}}), spec)
    assert moved and spec.gains["lead-wave14"] == 3.0
    spec2 = MixSpec({"lead-wave14": 0.0})
    assert adjust(reading(lead_vs_acc={"lead-wave14": {"median": 5.0, "worst": 1.0, "phrases": 4}}), spec2) == []


def test_fit_moves_the_low_group_to_its_target():
    spec = MixSpec({"sub808": 0.0, "bass-wave3": -8.0, "lead-wave14": 0.0})
    adjust(reading(vs_lead={"low": 0.0}), spec)
    assert spec.gains == {"sub808": -6.0, "bass-wave3": -14.0, "lead-wave14": 0.0}


def test_rhythm_stems_get_their_role_gain():
    from test_song_form import DM

    from sdsp_score.arrange import build_score
    from sdsp_score.form import parse_spec, song_form
    from sdsp_score.mix import initial_spec

    score = build_score(song_form(parse_spec("verse:8,chorus:8"), 72), DM)
    spec = initial_spec(score, ["lead-wave14", "drums", "sub808"])
    assert spec.gains == {"lead-wave14": 11.0, "drums": -6.0, "sub808": -6.0}


def test_fit_sets_loudness_last_through_the_limiter():
    spec = MixSpec({"lead-wave14": 0.0})
    assert adjust(reading(integrated=-26.0), spec, -14.0) and spec.limiter_in == 12.0
    spec2 = MixSpec({"sub808": 0.0, "lead-wave14": 0.0})
    adjust(reading(integrated=-26.0, vs_lead={"low": 0.0}), spec2, -14.0)
    assert spec2.limiter_in == 0.0  # balance first, loudness on a later pass
