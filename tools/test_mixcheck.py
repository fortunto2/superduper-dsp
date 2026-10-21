"""Known-answer checks for mixcheck.py: `python3 -m pytest tools/test_mixcheck.py`
(or run the file). Every meter here was fed a signal whose answer is known
before its zeros were trusted."""
import math
import random

import mixcheck

SR = 48000


def sine(hz, amp, seconds=5.0, phase=0.0):
    return [amp * math.sin(2 * math.pi * hz * i / SR + phase) for i in range(int(seconds * SR))]


def test_lufs_reads_ebu_tech_3341_case_1():
    # stereo 1 kHz sine at -23 dBFS must read -23.0 LUFS (EBU Tech 3341, case 1)
    x = sine(1000, 10 ** (-23 / 20))
    integrated, short_max = mixcheck.lufs(x, x, SR)
    assert abs(integrated + 23.0) < 0.1, integrated
    assert abs(short_max + 23.0) < 0.1, short_max


def test_lufs_gates_out_silence():
    # 5 s of tone then 5 s of silence: the gates drop the 46 silent blocks
    # (ungated it would read ~-26). The three blocks straddling the edge hold
    # 3/4, 1/2, 1/4 of the tone and pass the -10 LU gate, as the standard says:
    # 47 full + those three, by hand.
    x = sine(1000, 10 ** (-23 / 20)) + [0.0] * (5 * SR)
    expected = -23 + 10 * math.log10((47 + 0.75 + 0.5 + 0.25) / 50)
    assert abs(mixcheck.lufs(x, x, SR)[0] - expected) < 0.02


def test_mono_loss_in_phase_uncorrelated_and_cancelling():
    tone = sine(1000, 0.3)
    loss, _ = mixcheck.mono_loss(tone, tone, SR)
    assert abs(loss["mid"]) < 0.1, loss
    rnd = random.Random(1)
    a = [rnd.gauss(0, 0.1) for _ in range(5 * SR)]
    b = [rnd.gauss(0, 0.1) for _ in range(5 * SR)]
    loss, _ = mixcheck.mono_loss(a, b, SR)
    assert abs(loss["presence"] + 3.0) < 0.3, loss
    loss, _ = mixcheck.mono_loss(tone, [-v for v in tone], SR)
    assert loss["mid"] < -40, loss


def test_low_width_sees_wide_bass_only():
    bass = sine(60, 0.3)
    _, narrow = mixcheck.mono_loss(bass, bass, SR)
    _, wide = mixcheck.mono_loss(bass, sine(60, 0.3, phase=math.pi / 2), SR)
    assert narrow < -60 and wide > -1, (narrow, wide)


def test_profile_cuts_the_section(tmp_path=None):
    import pathlib, struct, tempfile, wave
    d = pathlib.Path(tmp_path or tempfile.mkdtemp())
    f = d / "two.wav"
    loud, quiet = sine(1000, 0.5, 2.0), sine(1000, 0.05, 2.0)
    with wave.open(str(f), "wb") as w:
        w.setnchannels(2); w.setsampwidth(2); w.setframerate(SR)
        w.writeframes(b"".join(struct.pack("<hh", int(v * 32767), int(v * 32767)) for v in loud + quiet))
    a, b = mixcheck.profile(f, 0, 2), mixcheck.profile(f, 2, 4)
    assert abs((a["rms_db"] - b["rms_db"]) - 20.0) < 0.2, (a["rms_db"], b["rms_db"])
    assert abs(a["seconds"] - 2.0) < 1e-3 and "bands_db" in a


if __name__ == "__main__":
    for name, f in list(globals().items()):
        if name.startswith("test_"):
            f()
            print("ok", name)


def test_target_corridor_is_level_independent_and_flags_one_band():
    """Three references with the same shape at three different levels give a
    tight corridor; a mix with +6 dB in the kick band is 'above' there and
    'inside' everywhere else, whatever its overall level."""
    import mixcheck as mc
    shape = {name: -10.0 - i for i, (_, _, name) in enumerate(mc.BANDS)}
    refs = [{"bands_db": {k: v + off for k, v in shape.items()}} for off in (0.0, -8.0, +5.0)]
    corridor = mc.target_corridor(refs)
    for _, _, name in mc.BANDS:
        assert corridor[name][1] == 1.0, "identical shapes: the spread is the floor"
    mix = {k: v - 20.0 for k, v in shape.items()}
    mix["kick"] += 6.0
    verdict = mc.target_verdict(mix, corridor)
    # the +6 dB kick also lifts the mean by 6/8, so the other bands sit
    # 0.75 dB low — inside the ±1 floor — and the kick is above by 6 − 0.75 − 1
    assert abs(verdict["kick"] - 4.25) < 1e-6, verdict
    assert all(v == 0.0 for k, v in verdict.items() if k != "kick"), verdict


def test_load_profiles_rejects_a_file_that_is_not_a_profile(tmp_path):
    import json
    import mixcheck as mc
    bad = tmp_path / "x.json"
    bad.write_text(json.dumps({"hello": 1}))
    try:
        mc.load_profiles([bad])
    except ValueError as e:
        assert "bands_db" in str(e)
    else:
        raise AssertionError("a profile without bands_db was accepted")
