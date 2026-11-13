import sys, collections
import mido
NAMES=['C','C#','D','D#','E','F','F#','G','G#','A','A#','B']
def nm(n): return f"{NAMES[n%12]}{n//12-1}"
m = mido.MidiFile(sys.argv[1])
print(f"type={m.type} tracks={len(m.tracks)} tpb={m.ticks_per_beat} len={m.length:.1f}s")
for i,tr in enumerate(m.tracks):
    name=None; prog=None; tempos=[]; ts=[]; notes=[]; ch=set()
    t=0; on={}
    for msg in tr:
        t+=msg.time
        if msg.type=='track_name': name=msg.name
        elif msg.type=='program_change': prog=msg.program; ch.add(msg.channel)
        elif msg.type=='set_tempo': tempos.append(mido.tempo2bpm(msg.tempo))
        elif msg.type=='time_signature': ts.append(f"{msg.numerator}/{msg.denominator}")
        elif msg.type=='note_on' and msg.velocity>0:
            on.setdefault(msg.note,[]).append(t); ch.add(msg.channel)
        elif msg.type in ('note_off',) or (msg.type=='note_on' and msg.velocity==0):
            if on.get(msg.note): notes.append((on[msg.note].pop(0), msg.note, t))
    if not notes and not tempos and not ts: continue
    print(f"\n-- track {i}: name={name!r} prog={prog} ch={sorted(ch)} notes={len(notes)}")
    if tempos: print(f"   tempo: {['%.3f'%x for x in tempos[:4]]} ts={ts}")
    if notes:
        pitches=[n[1] for n in notes]
        durs=[(n[2]-n[0])/m.ticks_per_beat for n in notes]
        c=collections.Counter(pitches)
        print(f"   range {nm(min(pitches))}..{nm(max(pitches))}  top: " +
              ", ".join(f"{nm(p)}×{k}" for p,k in c.most_common(6)))
        print(f"   dur(beats): med={sorted(durs)[len(durs)//2]:.2f} min={min(durs):.2f} max={max(durs):.2f}")
        pc=collections.Counter(p%12 for p in pitches)
        print(f"   pitch classes: " + ", ".join(f"{NAMES[p]}:{k}" for p,k in pc.most_common(7)))
        first=sorted(notes)[:8]
        print(f"   first onsets(beats): " + ", ".join(f"{n[0]/m.ticks_per_beat:.2f}@{nm(n[1])}" for n in first))
