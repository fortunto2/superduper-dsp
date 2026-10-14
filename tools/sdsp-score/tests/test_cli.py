"""The CLI: what it writes without rendering, and every refusal before audio is made."""

import json
import os
import stat

import pytest
from test_song_form import DM

from sdsp_score import config
from sdsp_score.cli import main


@pytest.fixture
def theme(tmp_path):
    p = tmp_path / "theme.json"
    p.write_text(json.dumps(DM.to_dict()))
    return p


def fake_bin(tmp_path, name, body):
    p = tmp_path / name
    p.write_text("#!/bin/sh\n" + body + "\n")
    p.chmod(p.stat().st_mode | stat.S_IEXEC)
    return p


@pytest.fixture
def spy_chain(tmp_path, monkeypatch):
    """An sdsp-chain that only records that it was called."""
    marker = tmp_path / "chain-called"
    monkeypatch.setenv("SDSP_CHAIN", str(fake_bin(tmp_path, "sdsp-chain", f"touch {marker}; exit 1")))
    return marker


def test_midi_only_writes_midi_toml_and_form(tmp_path, theme):
    out = tmp_path / "out"
    assert main([str(theme), "--form", "intro:4,verse:8,chorus:8,outro:4", "--out", str(out), "--midi-only"]) == 0
    for f in ("score.mid", "form.json", "score.json", "score.md"):
        assert (out / f).exists(), f
    assert list(out.glob("stem_lead-*.toml")) and list(out.glob("stem_bass-*.toml"))
    assert not list(out.glob("*.wav"))
    assert json.loads((out / "form.json").read_text())["sections"][2]["kind"] == "chorus"


def test_no_render_is_the_same_as_midi_only(tmp_path, theme):
    assert main([str(theme), "--length", "40", "--out", str(tmp_path / "o"), "--no-render"]) == 0


def test_a_saved_form_json_is_an_input(tmp_path, theme):
    out = tmp_path / "a"
    main([str(theme), "--length", "60", "--out", str(out), "--midi-only"])
    out2 = tmp_path / "b"
    assert main([str(theme), str(out / "form.json"), "--out", str(out2), "--midi-only"]) == 0
    assert (out / "score.mid").read_bytes() == (out2 / "score.mid").read_bytes()


def test_missing_sdsp_chain_is_exit_2_naming_the_binary(tmp_path, theme, monkeypatch, capsys):
    monkeypatch.setenv("SDSP_CHAIN", str(tmp_path / "no-such-sdsp-chain"))
    rc = main([str(theme), "--length", "40", "--out", str(tmp_path / "o")])
    assert rc == 2
    err = capsys.readouterr().err
    assert "sdsp-chain" in err and str(tmp_path / "no-such-sdsp-chain") in err


def test_an_unknown_kit_is_refused_before_any_stem(tmp_path, monkeypatch, spy_chain, capsys):
    d = DM.to_dict()
    d["style"]["kit"] = "kasette"
    p = tmp_path / "t.json"
    p.write_text(json.dumps(d))
    assert main([str(p), "--length", "40", "--out", str(tmp_path / "o")]) == 2
    assert "kasette" in capsys.readouterr().err
    assert not spy_chain.exists()


def test_missing_samples_are_refused_before_any_stem(tmp_path, theme, monkeypatch, spy_chain, capsys):
    monkeypatch.setenv("SDSP_SAMPLES", str(tmp_path / "nowhere"))
    assert main([str(theme), "--length", "40", "--out", str(tmp_path / "o")]) == 2
    assert "нет семпла" in capsys.readouterr().err
    assert not spy_chain.exists()


def test_a_bed_without_rubberband_is_refused(tmp_path, theme, monkeypatch, spy_chain, capsys):
    ff = fake_bin(tmp_path, "ffmpeg", 'echo " ... volume  A->A  Change input volume."')
    monkeypatch.setenv("SDSP_FFMPEG", str(ff))
    (tmp_path / "beat.wav").write_bytes(b"RIFF")
    form = {
        "length": 30,
        "sections": [{"start": 0, "end": 30, "kind": "view", "name": "01"}],
        "bed": {"path": "beat.wav", "key": "Dm", "bpm": 90},
    }
    (tmp_path / "f.json").write_text(json.dumps(form))
    assert main([str(theme), str(tmp_path / "f.json"), "--out", str(tmp_path / "o")]) == 2
    assert "rubberband" in capsys.readouterr().err
    assert not spy_chain.exists()


def test_a_bed_without_key_or_tempo_is_refused(tmp_path, theme, capsys):
    (tmp_path / "beat.wav").write_bytes(b"RIFF")
    form = {"length": 30, "sections": [{"start": 0, "end": 30, "kind": "view"}], "bed": {"path": "beat.wav"}}
    (tmp_path / "f.json").write_text(json.dumps(form))
    assert main([str(theme), str(tmp_path / "f.json"), "--out", str(tmp_path / "o")]) == 2
    assert "тональность или темп" in capsys.readouterr().err


@pytest.mark.parametrize("tmap", [[[0, 30, 0]], [[0, 30, 400]]])
def test_a_tempo_map_outside_40_200_is_exit_2(tmp_path, theme, tmap, capsys):
    form = {"length": 30, "sections": [{"start": 0, "end": 30, "kind": "view"}], "tempo_map": tmap}
    (tmp_path / "f.json").write_text(json.dumps(form))
    assert main([str(theme), str(tmp_path / "f.json"), "--out", str(tmp_path / "o"), "--midi-only"]) == 2
    assert "40..200" in capsys.readouterr().err


@pytest.mark.parametrize(
    "patch,word",
    [({"bpm": 300}, "40..200"), ({"chords": []}, "аккорд"), ({"motif": []}, "мотив"), ({"key": "H"}, "тональность")],
)
def test_a_bad_theme_is_exit_2(tmp_path, patch, word, capsys):
    d = {**DM.to_dict(), **patch}
    p = tmp_path / "t.json"
    p.write_text(json.dumps(d))
    assert main([str(p), "--length", "40", "--out", str(tmp_path / "o"), "--midi-only"]) == 2
    assert word in capsys.readouterr().err


def test_a_bad_form_spec_is_exit_2(tmp_path, theme, capsys):
    assert main([str(theme), "--form", "intro:8,solo:8", "--out", str(tmp_path / "o")]) == 2
    assert "solo" in capsys.readouterr().err


def test_check_without_a_render_is_exit_2(tmp_path):
    assert main(["check", str(tmp_path)]) == 2


def test_paths_come_from_env(monkeypatch, tmp_path):
    monkeypatch.setenv("SDSP_SAMPLES", str(tmp_path))
    monkeypatch.setenv("SDSP_CHAIN", "/x/sdsp-chain")
    assert config.samples_dir() == tmp_path and config.chain_bin() == "/x/sdsp-chain"
    assert os.path.basename(str(config.samples_dir())) == tmp_path.name
