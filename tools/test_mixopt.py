"""Known-answer checks for mixopt.py: `python3 -m pytest tools/test_mixopt.py -s`.

Every meter is fed a signal whose answer is known before its zeros are trusted.
The three controls from the brief need the renderer and skip, saying so, when
`sdsp-chain` is not built (`cargo build --release -p sdsp-chain`, or SDSP_CHAIN=).
"""
import math
import struct
import subprocess
import sys
from pathlib import Path

import mixopt
import numpy as np
import pytest
import tomllib

SR = 48000


def tone(hz, amp, seconds=4.0):
    t = np.arange(int(seconds * SR)) / SR
    return amp * np.sin(2 * np.pi * hz * t)


def write_wav(path, mono):
    """16-bit stereo PCM, both channels the same."""
    pcm = (np.clip(mono, -1, 1) * 32767).astype("<i2")
    data = np.repeat(pcm, 2).tobytes()
    hdr = struct.pack("<4sI4s4sIHHIIHH4sI", b"RIFF", 36 + len(data), b"WAVE", b"fmt ", 16,
                      1, 2, SR, SR * 4, 4, 16, b"data", len(data))
    Path(path).write_bytes(hdr + data)


@pytest.fixture
def chain():
    c = mixopt.find_chain()
    if not c:
        pytest.skip("sdsp-chain not built: cargo build --release -p sdsp-chain (or set SDSP_CHAIN)")
    return c


def chord_mix(d, mid_gain=0.0):
    """A chord stem (A3 C#4 E4) over a bass stem, one EQ on the chord at 300 Hz."""
    write_wav(d / "gtr.wav", sum(tone(hz, 0.12) for hz in (220.0, 277.18, 329.63)))
    write_wav(d / "bass.wav", tone(55.0, 0.25) + tone(110.0, 0.08))
    return {"out": "mix.wav", "track": [
        {"name": "gtr", "input": "gtr.wav", "gain_db": -3.0,
         "stage": [{"plugin": "eq", "params": {"Mid Freq": 300.0, "Mid Gain": mid_gain, "Mid Q": 0.7}}]},
        {"name": "bass", "input": "bass.wav"}],
        "master": [{"plugin": "limiter", "params": {"Ceiling": -1.0}}]}


def render_ref(chain, cfg, d):
    return mixopt.render(chain, mixopt._absolutise(cfg, d), d / "ref.toml", d / "ref.wav")


# -- meters, no renderer needed -------------------------------------------------

def test_bus_band_meter_reads_a_tone_at_its_rms():
    # −20 dBFS RMS sine at 300 Hz: the 266–331 Hz bus band must read −20 dB (band RMS,
    # Parseval-normalised like gui.rs::scope_spectrum), and two bands up must sit
    # under the −50 dB LOUD threshold, or every pair would "clash" on window leakage.
    b = mixopt.bus_bands(tone(300.0, 10 ** (-20 / 20) * math.sqrt(2)), SR)
    edges = mixopt.bus_band_edges()
    k = int(np.searchsorted(edges, 300.0) - 1)
    assert 265 <= edges[k] <= 267 and 330 <= edges[k + 1] <= 332, edges[k:k + 2]
    assert abs(b[:, k].mean() + 20.0) < 0.3, b[:, k].mean()
    assert b[:, k + 2].max() < -50, b[:, k + 2].max()


def test_toml_writer_round_trips_the_chain_schema():
    cfg = {"out": "x.wav", "tail_s": 1.5, "track": [
        {"name": "voice pad", "input": "a.wav", "gain_automate": [[0.0, -60.0], [6.0, -6.0]],
         "stage": [{"plugin": "eq", "params": {"Mid Gain": -2.0, "3": 1.0, "HP": 30},
                    "automate": {"Mix": [[0.0, 0.0], [4.0, 0.95]]}, "sidechain": "track:kick"}]},
        {"name": "kick", "input": "k.wav", "mute": True, "stage": []}],
        "master": [{"plugin": "limiter", "params": {"Ceiling": -1.0}}]}
    assert tomllib.loads(mixopt.dump_toml(cfg)) == cfg


def test_param_table_parses_names_with_spaces(chain):
    rows = {r["name"]: r for r in mixopt.param_table(chain, "eq")}
    assert rows["Mid Gain"]["min"] == -15.0 and rows["Mid Gain"]["max"] == 15.0
    assert rows["Mid Freq"]["default"] == 1000.0 and rows["Mid Freq"]["unit"] == "Hz"


# -- the three controls from the brief -------------------------------------------

def test_identity_loss_is_zero_and_the_optimizer_stays_put(chain, tmp_path):
    cfg = chord_mix(tmp_path)
    ref = render_ref(chain, cfg, tmp_path)
    targets = [mixopt.parse_vary("gtr.eq.Mid Gain:-12:12", cfg, chain)]
    terms, _ = mixopt.parse_loss("ref:ref.wav,loudness")
    ev = mixopt.Evaluator(chain, cfg, tmp_path, targets, terms, str(ref), tmp_path / "w")
    loss0, t0 = ev.measure([0.0])
    assert loss0 < 1e-6, t0
    best, best_loss, hist = mixopt.optimize(ev, targets, iters=20, quiet=True)
    print(f"\nidentity: base loss {loss0:.2e}, best {best[0]:+.3f} dB after {len(hist)} evaluations, "
          f"{np.mean(ev.seconds):.2f} s each")
    assert abs(best[0]) < 0.5 and best_loss < 1e-6, (best, best_loss)


def test_planted_boost_is_undone_within_40_evaluations(chain, tmp_path):
    ref = render_ref(chain, chord_mix(tmp_path, mid_gain=0.0), tmp_path)
    cfg = chord_mix(tmp_path, mid_gain=8.0)  # the defect: +8 dB at 300 Hz on the chord
    targets = [mixopt.parse_vary("gtr.eq.Mid Gain:-12:12", cfg, chain)]
    terms, _ = mixopt.parse_loss("ref:ref.wav")
    ev = mixopt.Evaluator(chain, cfg, tmp_path, targets, terms, str(ref), tmp_path / "w")
    best, best_loss, hist = mixopt.optimize(ev, targets, iters=40, quiet=True)
    base_loss = hist[0]["loss"]
    print(f"\nplanted +8 dB: loss {base_loss:.4f} → {best_loss:.4f} "
          f"({100 * (1 - best_loss / base_loss):.0f}% down), Mid Gain +8 → {best[0]:+.2f} dB, "
          f"{len(hist)} evaluations, {np.mean(ev.seconds):.2f} s each")
    assert len(hist) <= 40
    assert best[0] <= 2.0, best            # undid at least 6 of the 8 dB
    assert best_loss <= 0.3 * base_loss, (base_loss, best_loss)


def test_masking_share_falls_under_a_10_db_cut(chain, tmp_path):
    amp = 10 ** (-20 / 20) * math.sqrt(2)
    write_wav(tmp_path / "a.wav", tone(300.0, amp))
    write_wav(tmp_path / "b.wav", tone(305.0, amp))
    cfg = {"track": [{"name": "a", "input": "a.wav"},
                     {"name": "b", "input": "b.wav", "stage": [
                         {"plugin": "eq", "params": {"Mid Freq": 300.0, "Mid Gain": 0.0, "Mid Q": 0.7}}]}]}
    targets = [mixopt.parse_vary("b.eq.Mid Gain:-15:0", cfg, chain)]
    terms, _ = mixopt.parse_loss("masking")
    ev = mixopt.Evaluator(chain, cfg, tmp_path, targets, terms, work=tmp_path / "w")
    before, _ = ev.measure([0.0])
    worst_before = ev.last_masking[0]
    after, _ = ev.measure([-10.0])
    worst_after = ev.last_masking[0]
    print(f"\nmasking a×b at {worst_before['hz']} Hz: {100 * worst_before['worst_share']:.0f}% → "
          f"{100 * worst_after['worst_share']:.0f}% (Σ {before:.2f} → {after:.2f})")
    assert worst_before["worst_share"] > 0.5 and 265 <= worst_before["hz"][0] <= 267, worst_before
    assert worst_after["worst_share"] < 0.2, worst_after
    assert after < before


# -- the level penalty and the CLI ------------------------------------------------

def test_level_penalty_charges_a_fader_drop_past_2_db_but_not_a_narrow_bell(chain, tmp_path):
    # Five octaves of tone so the stem carries energy in five bus bands: a fader
    # move drops all of them (the median sees it), a Q 3 bell at 440 Hz drops one.
    write_wav(tmp_path / "pad.wav", sum(tone(hz, 0.1) for hz in (110.0, 220.0, 440.0, 880.0, 1760.0)))
    cfg = {"track": [{"name": "pad", "input": "pad.wav", "gain_db": -3.0, "stage": [
        {"plugin": "eq", "params": {"Mid Freq": 440.0, "Mid Gain": 0.0, "Mid Q": 3.0}}]}]}
    targets = [mixopt.parse_vary("pad.gain_db:-12:0", cfg, chain),
               mixopt.parse_vary("pad.eq.Mid Gain:-15:0", cfg, chain)]
    terms, _ = mixopt.parse_loss("level")
    ev = mixopt.Evaluator(chain, cfg, tmp_path, targets, terms, work=tmp_path / "w")
    assert ev.measure([-3.0, 0.0])[1]["level"] == pytest.approx(0.0, abs=1e-6)   # the base itself
    assert ev.measure([-4.5, 0.0])[1]["level"] == pytest.approx(0.0, abs=0.05)   # 1.5 dB down: free
    assert ev.measure([-8.0, 0.0])[1]["level"] == pytest.approx(3.0, abs=0.05)   # 5 dB down: pays 3
    bell = ev.measure([-3.0, -10.0])[1]
    print(f"\nlevel: −10 dB bell at 440 Hz Q 3 reads {bell['_levels']['pad']:+.2f} dB broadband, "
          f"term {bell['level']:.3f}; −5 dB fader pays 3.0")
    assert bell["level"] == pytest.approx(0.0, abs=1e-6), bell                   # narrow cut: free


def test_cli_writes_best_toml_render_and_log(chain, tmp_path):
    ref = render_ref(chain, chord_mix(tmp_path, 0.0), tmp_path)
    (tmp_path / "base.toml").write_text(mixopt.dump_toml(chord_mix(tmp_path, 8.0)))
    r = subprocess.run([sys.executable, str(Path(mixopt.__file__)), str(tmp_path / "base.toml"),
                        "--vary", "gtr.eq.Mid Gain:-12:12", "--loss", f"ref:{ref},loudness",
                        "--iters", "8", "--out", str(tmp_path / "best.toml"), "--chain", chain],
                       capture_output=True, text=True, check=False)
    assert r.returncode == 0, r.stdout + r.stderr
    best = tomllib.loads((tmp_path / "best.toml").read_text())
    assert best["track"][0]["stage"][0]["params"]["Mid Gain"] < 8.0
    assert best["track"][0]["input"] == "gtr.wav"            # relative paths kept next to the base
    assert (tmp_path / "best.wav").exists()
    lines = (tmp_path / "best.jsonl").read_text().splitlines()
    assert len(lines) == 9 and "loss_spec" in lines[0] and '"diff"' in lines[-1]
    assert "Listen before keeping" in r.stdout
