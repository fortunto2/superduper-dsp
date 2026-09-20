#!/usr/bin/env python
"""How well do a MIDI track's onsets sit on the beat grid?

    uvx --with mido python gridcheck.py file.mid [track_name] [note]

Prints hits, missing beats and deviation in ms. A transcription that reports
the right notes on a wrong grid is useless for rebuilding, and only this
tells them apart.
"""
import sys
import mido

path = sys.argv[1]
want_track = sys.argv[2] if len(sys.argv) > 2 else "drums"
want_note = int(sys.argv[3]) if len(sys.argv) > 3 else 36  # GM kick

m = mido.MidiFile(path)
tpb = m.ticks_per_beat
bpm = next((mido.tempo2bpm(msg.tempo) for tr in m.tracks for msg in tr
            if msg.type == "set_tempo"), 120.0)

hits = []
for tr in m.tracks:
    if want_track.lower() not in (tr.name or "").lower():
        continue
    t = 0
    for msg in tr:
        t += msg.time
        if msg.type == "note_on" and msg.velocity > 0 and msg.note == want_note:
            hits.append(t / tpb)

if not hits:
    print(f"no note {want_note} on a track matching {want_track!r} — nothing measured")
    sys.exit(2)

hits.sort()
dev_ms = sorted((h - round(h)) * 60000 / bpm for h in hits)
beats = {round(h) for h in hits}
lo, hi = int(hits[0]), int(hits[-1])
missing = [b for b in range(lo, hi + 1) if b not in beats]

span = hi - lo + 1
covered = len(beats & set(range(lo, hi + 1)))
print(f"{len(hits)} hits, beats {lo}..{hi} at {bpm:.3f} BPM")
print(f"offset from grid: median {dev_ms[len(dev_ms)//2]:+.1f} ms, "
      f"max |dev| {max(abs(d) for d in dev_ms):.1f} ms")
print(f"beats covered {covered}/{span}, missing {len(missing)}: {missing[:30]}")
if len(hits) > covered:
    print(f"note: {len(hits) - covered} extra hit(s) — doubles, or onsets off by "
          f"more than half a beat")
