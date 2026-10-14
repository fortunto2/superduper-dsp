"""score.mid — every part as its own track, drums on channel 10 in the GM map.

Type-1 SMF, 480 ticks per quarter. Track 0 carries the tempo map (one tempo
event per phrase whose tempo differs from the previous), so bars line up in
any DAW even though the score is written in seconds. Track names are our
part names (`lead-wave14`, `ostinato-kubyz1`): instrument and preset survive
the round trip into sdsp-chain or REAPER. No dependencies.
"""

import bisect
import struct
from pathlib import Path

from . import config
from .music import role_of

PPQ = 480
DRUM_CH = 9


def _vlq(n: int) -> bytes:
    out = [n & 0x7F]
    n >>= 7
    while n:
        out.append((n & 0x7F) | 0x80)
        n >>= 7
    return bytes(reversed(out))


def _meta(typ: int, data: bytes) -> bytes:
    return bytes([0xFF, typ]) + _vlq(len(data)) + data


def _track(events: list[tuple[int, int, bytes]]) -> bytes:
    """events: [(tick, order, message)] → MTrk chunk; order puts note-offs before note-ons."""
    body, last = b"", 0
    for tick, _o, msg in sorted(events, key=lambda e: (e[0], e[1])):
        body += _vlq(tick - last) + msg
        last = tick
    body += b"\x00\xff\x2f\x00"
    return b"MTrk" + struct.pack(">I", len(body)) + body


class TempoMap:
    def __init__(self, changes: list[tuple[float, float]]):
        """changes: [(second, bpm)], sorted, first at 0."""
        self.secs, self.bpms, self.ticks = [], [], []
        tick = 0.0
        for t, bpm in changes:
            if self.bpms and abs(bpm - self.bpms[-1]) < 1e-6:
                continue
            if self.secs:
                tick += (t - self.secs[-1]) * self.bpms[-1] / 60 * PPQ
            self.secs.append(t)
            self.bpms.append(bpm)
            self.ticks.append(tick)

    def tick(self, t: float) -> int:
        i = max(0, bisect.bisect_right(self.secs, t) - 1)
        return round(self.ticks[i] + (t - self.secs[i]) * self.bpms[i] / 60 * PPQ)


def tempo_changes(score) -> list[tuple[float, float]]:
    ch = [(0.0, 60.0 / score.phrases[0].beat)] if score.phrases else [(0.0, 120.0)]
    ch += [(p.start, 60.0 / p.beat) for p in score.phrases]
    return ch


def write(path, score, hits=(), subs=()) -> list[str]:
    """Write score.mid; returns the track names in file order (after the tempo track)."""
    cfg = config.cfg()
    tm = TempoMap(tempo_changes(score))
    tempo_ev = [(0, 0, _meta(0x03, b"tempo")), (0, 0, _meta(0x58, bytes([4, 2, 24, 8])))]
    for bpm, tk in zip(tm.bpms, tm.ticks):
        tempo_ev.append((round(tk), 1, _meta(0x51, round(60e6 / bpm).to_bytes(3, "big"))))
    tracks = [_track(tempo_ev)]
    names: list[str] = []
    parts = [(t, ns) for t, ns in score.notes.items() if ns]
    if subs:
        parts.append(("sub808", [list(s) for s in subs]))
    ch = 0
    for name, notes in parts:
        if ch == DRUM_CH:
            ch += 1
        c = ch % 16
        ch += 1
        prog = cfg["gm_program"].get(role_of(name), 0)
        ev = [(0, 0, _meta(0x03, name.encode())), (0, 0, bytes([0xC0 | c, prog]))]
        for at, dur, pitch, vel in notes:
            a, b = tm.tick(at), max(tm.tick(at) + 1, tm.tick(at + dur))
            ev.append((a, 2, bytes([0x90 | c, int(pitch) & 0x7F, max(1, min(127, int(vel)))])))
            ev.append((b, 1, bytes([0x80 | c, int(pitch) & 0x7F, 0])))
        tracks.append(_track(ev))
        names.append(name)
    if hits:
        gm = cfg["gm_drums"]
        ev = [(0, 0, _meta(0x03, b"drums"))]
        for t, part, v in hits:
            p = part.split(":")[-1]
            note = gm[p]
            if p == "tom":  # three tom heights like the audio fill
                note = {0: 45, 1: 47, 2: 48}[int(t * 8) % 3]
            a = tm.tick(t)
            ev.append((a, 2, bytes([0x90 | DRUM_CH, note, max(1, min(127, int(v)))])))
            ev.append((a + PPQ // 8, 1, bytes([0x80 | DRUM_CH, note, 0])))
        tracks.append(_track(ev))
        names.append("drums")
    head = b"MThd" + struct.pack(">IHHH", 6, 1, len(tracks), PPQ)
    Path(path).write_bytes(head + b"".join(tracks))
    return names


def summary(path) -> list[dict]:
    """Read back: [{name, channels, notes: [(tick, pitch, vel)]}] per track, tempo track included."""
    data = Path(path).read_bytes()
    if data[:4] != b"MThd":
        raise ValueError(f"{path} — не MIDI")
    _hl, _fmt, ntrk, _div = struct.unpack(">IHHH", data[4:14])
    pos, out = 14, []
    for _ in range(ntrk):
        ln = struct.unpack(">I", data[pos + 4 : pos + 8])[0]
        p, end, tick, status = pos + 8, pos + 8 + ln, 0, 0
        tr = {"name": "", "channels": set(), "notes": [], "tempi": []}
        while p < end:
            n = 0
            while True:
                b = data[p]
                p += 1
                n = (n << 7) | (b & 0x7F)
                if not b & 0x80:
                    break
            tick += n
            b = data[p]
            if b == 0xFF:
                typ, p = data[p + 1], p + 2
                ln2 = 0
                while True:
                    c = data[p]
                    p += 1
                    ln2 = (ln2 << 7) | (c & 0x7F)
                    if not c & 0x80:
                        break
                if typ == 0x03:
                    tr["name"] = data[p : p + ln2].decode()
                elif typ == 0x51:
                    tr["tempi"].append((tick, 60e6 / int.from_bytes(data[p : p + 3], "big")))
                p += ln2
                continue
            if b & 0x80:
                status, p = b, p + 1
            hi = status & 0xF0
            one = hi in (0xC0, 0xD0)
            a, c = data[p], (0 if one else data[p + 1])
            p += 1 if one else 2
            if hi == 0x90 and c > 0:
                tr["channels"].add(status & 0x0F)
                tr["notes"].append((tick, a, c))
        out.append(tr)
        pos = end
    return out
