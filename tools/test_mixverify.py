"""Known-answer controls for mixverify.py: `python3 -m pytest tools/test_mixverify.py`.

The fixture is a two-track mix with a planted clash: a bass at 55 Hz whose
5th and 6th harmonics (275, 330 Hz) sit in the same band as a chord
cluster's lowest tone (300 Hz), within 6 dB of it — what the bus calls
masking. "After" cuts the chord 6 dB at 300 Hz (sdsp-chain `eq`, Mid Freq /
Mid Gain / Mid Q). The bus side is a fake mix-log, because the controls are
about the rule, not about the bus: the rule must say KEEP when both
instruments agree, UNKNOWN when nothing moved, UNDO when the log claims a
win the render cannot see.

Needs the sdsp-chain binary; without it the render controls skip and say so.
"""
import json
import math
import struct
import wave
from pathlib import Path

import mixverify
import pytest

SR = 48000
SECONDS = 4.0
BAND = (250.0, 340.0)
NO_CHAIN = ("sdsp-chain not found: `cargo build --release -p sdsp-chain` "
            "(CARGO_TARGET_DIR=... or set SDSP_CHAIN=/path/to/sdsp-chain)")


def tones(parts):
    n = int(SECONDS * SR)
    out = [0.0] * n
    for hz, amp in parts:
        for i in range(n):
            out[i] += amp * math.sin(2 * math.pi * hz * i / SR)
    return out


def write_wav(path, x):
    with wave.open(str(path), "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(b"".join(struct.pack("<hh", int(v * 32767), int(v * 32767)) for v in x))


BEFORE = """\
[[track]]
name = "bass"
input = "bass.wav"

[[track]]
name = "chord"
input = "chord.wav"

[[master]]
plugin = "eq"
"""

CUT_300 = """\
  [[track.stage]]
  plugin = "eq"
  params = { "Mid Freq" = 300.0, "Mid Gain" = -6.0, "Mid Q" = 2.0 }
"""


def clash(share):
    return {"a": "bass", "b": "chord", "low_hz": BAND[0], "high_hz": BAND[1],
            "share": share, "louder": "bass", "by_db": 2.3}


def run_line(label, share, chord_db=-31.8):
    run = {"label": label, "seconds": 4.0, "tracks": ["bass", "chord"],
           "clashes": [clash(share)] if share else [], "buried": [],
           "levels": [{"track": "Master", "rms_db": -12.0}, {"track": "bass", "rms_db": -13.0},
                      {"track": "chord", "rms_db": chord_db}]}
    return json.dumps(run, ensure_ascii=False)


@pytest.fixture(scope="module")
def mix(tmp_path_factory):
    d = Path(tmp_path_factory.mktemp("mixverify"))
    # bass: 55 Hz with harmonics 1/k — the 5th (275) and 6th (330) land in BAND
    write_wav(d / "bass.wav", tones([(55 * k, 0.4 / k) for k in range(1, 7)]))
    # chord cluster: 300 Hz is the planted overlap, within 6 dB of the bass there
    write_wav(d / "chord.wav", tones([(300, 0.08), (450, 0.08), (600, 0.08)]))
    (d / "before.toml").write_text(BEFORE)
    (d / "after.toml").write_text(BEFORE.replace('input = "chord.wav"\n', 'input = "chord.wav"\n' + CUT_300))
    (d / "same.toml").write_text(BEFORE)
    (d / "mix-log.jsonl").write_text("\n".join([
        run_line("before", 0.72), run_line("after", 0.30, chord_db=-32.9),
        run_line("same-a", 0.72), run_line("same-b", 0.72),
    ]) + "\n")
    return d


def verify(mix, before, after, labels, **extra):
    if mixverify.find_chain() is None:
        pytest.skip(NO_CHAIN)
    argv = [str(mix / before), str(mix / after), "--question", "masking",
            "--mix-log", str(mix / "mix-log.jsonl"), "--keep", str(mix / "renders")]
    if labels:
        argv += ["--labels", labels]
    for k, v in extra.items():
        argv += [f"--{k}", str(v)]
    return mixverify.run(mixverify.parse_args(argv))


# ---- the three controls -----------------------------------------------------

def test_keep_when_render_and_bus_agree(mix):
    res = verify(mix, "before.toml", "after.toml", "before,after")
    rend, bus = res["render"]["reading"], res["bus"]["reading"]
    assert res["verdict"] == "KEEP", res["reasons"]
    # the bus fell 42 pt; the stems' separation at 250-340 Hz grew by ~6 dB
    assert bus["delta"] == pytest.approx(-0.42, abs=1e-6)
    assert 4.0 < rend["delta"] < 8.0, rend
    # the yielding stem is the chord, and the bass did not move there
    chord_d = res["render"]["after"]["stems"]["chord"]["band_db"] - res["render"]["before"]["stems"]["chord"]["band_db"]
    bass_d = res["render"]["after"]["stems"]["bass"]["band_db"] - res["render"]["before"]["stems"]["bass"]["band_db"]
    assert -8.0 < chord_d < -4.0 and abs(bass_d) < 0.2, (chord_d, bass_d)
    assert res["guards"] == []


def test_unknown_when_nothing_changed(mix):
    res = verify(mix, "before.toml", "same.toml", "same-a,same-b")
    assert res["verdict"] == "UNKNOWN", res["reasons"]
    assert any("no change on either instrument" in r for r in res["reasons"])
    assert res["render"]["reading"]["delta"] == pytest.approx(0.0, abs=1e-9)


def test_undo_when_bus_claims_a_win_the_render_cannot_see(mix):
    # same render, but the log says the clash fell 72 % → 30 %
    res = verify(mix, "before.toml", "same.toml", "before,after")
    assert res["verdict"] == "UNDO", res["reasons"]
    assert any("disagree" in r for r in res["reasons"])
    assert res["bus"]["reading"]["move"] == 1 and res["render"]["reading"]["move"] == 0


# ---- the rule's other exits, no render needed --------------------------------

def test_unknown_without_labels_never_keeps_on_one_instrument(mix):
    res = verify(mix, "before.toml", "after.toml", None)
    assert res["verdict"] == "UNKNOWN"
    assert "one instrument is not an argument" in res["reasons"][0]


def test_mono_question_has_no_bus_instrument(mix):
    res = verify(mix, "before.toml", "after.toml", "before,after", question="mono")
    assert res["verdict"] == "UNKNOWN"
    assert "mix_phase" in res["reasons"][0]


def test_table_prints_both_instruments_and_json_is_finite(mix, capsys):
    if mixverify.find_chain() is None:
        pytest.skip(NO_CHAIN)
    argv = [str(mix / "before.toml"), str(mix / "after.toml"), "--labels", "before,after",
            "--mix-log", str(mix / "mix-log.jsonl")]
    assert mixverify.main(argv) == "KEEP"
    out = capsys.readouterr().out
    assert "instrument 1" in out and "instrument 2" in out and "VERDICT: KEEP" in out
    assert mixverify.main(argv + ["--json"]) == "KEEP"
    doc = json.loads(capsys.readouterr().out)
    assert doc["verdict"] == "KEEP" and doc["thresholds"] == {"level_db": 1.0, "band_db": 1.5, "share": 0.1}


def test_stem_toml_mutes_the_others_and_drops_the_master():
    text = BEFORE.replace('input = "chord.wav"\n', 'input = "chord.wav"\nmute = false\n' + CUT_300)
    stem = mixverify.stem_toml(text, 1)
    assert "[[master]]" not in stem
    blocks = stem.split("[[track]]")[1:]
    assert "mute = true" in blocks[0] and "mute" not in blocks[1]
    assert "[[track.stage]]" in blocks[1] and '"Mid Gain" = -6.0' in blocks[1]


def test_verdict_rule_table():
    ok = lambda d, t=1.0: mixverify.reading(0.0, d, t, "dB", "x")
    assert mixverify.verdict("masking", ok(2.0), ok(2.0), [])[0] == "KEEP"
    assert mixverify.verdict("masking", ok(-2.0), ok(-2.0), [])[0] == "UNDO"
    assert mixverify.verdict("masking", ok(2.0), ok(0.5), [])[0] == "UNDO"
    assert mixverify.verdict("masking", ok(0.0), ok(0.0), [])[0] == "UNKNOWN"
    assert mixverify.verdict("masking", ok(2.0), ok(2.0), [("bus", "x grew")])[0] == "UNDO"
    assert mixverify.verdict("masking", mixverify.missing("no log"), ok(2.0), [])[0] == "UNKNOWN"
    assert mixverify.verdict("balance", ok(-2.0), ok(-2.0), [])[0] == "KEEP"
    assert mixverify.verdict("balance", ok(-2.0), ok(-2.0), [], expect="up")[0] == "UNDO"
    assert mixverify.verdict("loudness", ok(2.0), ok(-2.0), [])[0] == "UNDO"
