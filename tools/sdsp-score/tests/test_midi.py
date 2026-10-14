"""score.mid: a track per part, drums on channel 10 in the GM map, the tempo map in track 0."""

import struct

from test_song_form import DM

from sdsp_score import drums as td
from sdsp_score import form as fm
from sdsp_score import midi
from sdsp_score.arrange import build_score
from sdsp_score.music import motif_from_midi


def written(tmp_path):
    form = fm.song_form(fm.parse_spec(fm.DEFAULT_FORM), DM.bpm)
    score = build_score(form, DM)
    hits, subs = td.drum_score(score.phrases)
    names = midi.write(tmp_path / "score.mid", score, hits, subs)
    return score, hits, subs, names, midi.summary(tmp_path / "score.mid")


def test_every_part_is_its_own_track(tmp_path):
    score, hits, subs, names, tracks = written(tmp_path)
    parts = [t for t, ns in score.notes.items() if ns]
    assert names == [*parts, "sub808", "drums"]
    assert [t["name"] for t in tracks] == ["tempo", *names]
    for t in tracks[1:]:
        want = len(score.notes.get(t["name"], subs if t["name"] == "sub808" else hits))
        assert len(t["notes"]) == want, t["name"]
        if t["name"] != "drums":
            assert 9 not in t["channels"]


def test_drums_are_gm_on_channel_ten(tmp_path):
    *_, tracks = written(tmp_path)
    drums = next(t for t in tracks if t["name"] == "drums")
    assert drums["channels"] == {9}
    pitches = {p for _, p, _ in drums["notes"]}
    assert {36, 38, 42} <= pitches  # kick, backbeat, closed hat
    assert pitches <= {35, 36, 38, 39, 42, 46, 45, 47, 48, 49}


def test_the_tempo_map_follows_the_sections(tmp_path):
    score, *_, tracks = written(tmp_path)
    tempi = [round(b, 2) for _, b in tracks[0]["tempi"]]
    assert sorted(set(tempi)) == sorted({round(s.bpm, 2) for s in score.sections})
    # the first chorus note lands on the bar its section starts on
    chorus = next(s for s in score.sections if s.kind == "chorus")
    tm = midi.TempoMap(midi.tempo_changes(score))
    bars = sum(round((s.end - s.start) / (240 / s.bpm)) for s in score.sections if s.end <= chorus.start)
    assert abs(tm.tick(chorus.start) - bars * 4 * midi.PPQ) <= 1


def _vlq(n):
    out = [n & 0x7F]
    n >>= 7
    while n:
        out.append((n & 0x7F) | 0x80)
        n >>= 7
    return bytes(reversed(out))


def write_midi(path, notes, tpb=480):
    """notes: [(t_s, dur_s, pitch)] at 120 bpm."""
    ev = []
    for t, d, p in notes:
        ev.append((round(t * tpb * 2), bytes([0x90, p, 80])))
        ev.append((round((t + d) * tpb * 2), bytes([0x80, p, 0])))
    ev.sort(key=lambda e: (e[0], e[1][0] == 0x90))
    body, last = b"", 0
    for tick, msg in ev:
        body += _vlq(tick - last) + msg
        last = tick
    body += b"\x00\xff\x2f\x00"
    data = b"MThd" + struct.pack(">IHHH", 6, 0, 1, tpb) + b"MTrk" + struct.pack(">I", len(body)) + body
    path.write_bytes(data)


def test_the_motif_is_the_most_repeated_figure(tmp_path):
    figure = [62, 65, 67, 69, 67, 65]  # D F G A G F, four times, noise between
    notes, t = [], 0.0
    noise = [[60, 70, 58], [72, 57], [64, 59, 71], [55]]
    for k in range(4):
        for p in figure:
            notes.append((t, 0.25, p))
            t += 0.3
        t += 1.0
        for p in noise[k]:
            notes.append((t, 0.25, p))
            t += 0.3
        t += 1.0
    write_midi(tmp_path / "x.mid", notes)
    motif = motif_from_midi(tmp_path / "x.mid", key_pc=2, minor=True)
    assert [d for d, _ in motif] == [1, 3, 4, 5, 4, 3]
