"""Parity of the Rust transcriber against the Python references (the known answers).

Run from guitar-hear (its env has basic-pitch, mir_eval):
    cd ~/Music/1music/guitar-hear && ./run.sh ../superduper-dsp/transcribe/parity/parity.py WORKDIR
WORKDIR holds theme2015.wav plus the CLI outputs g.json (--mode guitar --capo 1 --maxfret 5),
any.json (--mode any) and raw/{note,onset}.f32 (--raw). See parity/README.md.
"""
import json, sys
from pathlib import Path
import numpy as np, mir_eval

W = Path(sys.argv[1]); GH = Path(__file__).resolve().parents[3] / "guitar-hear"
sys.path.insert(0, str(GH))

d = np.load(GH / "cache/theme2015.npz", allow_pickle=True)
rn = np.fromfile(W / "raw/note.f32", dtype="<f4").reshape(-1, 88)
ro = np.fromfile(W / "raw/onset.f32", dtype="<f4").reshape(-1, 88)
n = min(len(rn), len(d["note"]))
print(f"posteriors: frames rust {len(rn)} python {len(d['note'])}")
for name, a, b in (("note", rn, d["note"]), ("onset", ro, d["onset"])):
    e = np.abs(a[:n] - b[:n]); print(f"  {name}: max |diff| {e.max():.4f}, mean {e.mean():.5f}, p99 {np.percentile(e,99):.4f}")

def onset_f1(ref, est, win=0.05):
    ref = np.array(ref); est = np.array(est)
    if len(ref) == 0 or len(est) == 0: return 0, 0, 0
    # match by (pitch, onset within win)
    used = set(); tp = 0
    for i, (t, m) in enumerate(est):
        best = None
        for j, (t2, m2) in enumerate(ref):
            if j in used or m != m2 or abs(t - t2) > win: continue
            if best is None or abs(t - t2) < abs(t - ref[best][0]): best = j
        if best is not None: used.add(best); tp += 1
    p = tp / len(est); r = tp / len(ref); return (2 * p * r / (p + r) if tp else 0), p, r

# --- Guitar vs gh.py committed output
py = json.load(open(GH / "cache/theme2015.gh.capo1.json")); rs = json.load(open(W / "g.json"))
f1, p, r = onset_f1([(x["start"], x["midi"]) for x in py], [(x["start"], x["midi"]) for x in rs])
print(f"guitar: python {len(py)} notes, rust {len(rs)}; onset+pitch F1@50ms {f1:.3f} (P {p:.3f} R {r:.3f})")
used = set(); agree = tot = fr = 0
for x in rs:
    c = [(abs(x["start"] - y["start"]), j) for j, y in enumerate(py) if j not in used and y["midi"] == x["midi"] and abs(x["start"] - y["start"]) <= 0.05]
    if c:
        _, j = min(c); used.add(j); tot += 1; agree += x["string"] == py[j]["string"]; fr += x["fret"] == py[j]["fret"]
print(f"  matched {tot}: same string {agree}, same fret {fr}")
dend = [abs(x["end"] - py[j]["end"]) for x in rs for j in [min(range(len(py)), key=lambda j: abs(py[j]["start"] - x["start"]) + 99 * (py[j]["midi"] != x["midi"]))] if py[j]["midi"] == x["midi"]]
print(f"  median |end diff| {np.median(dend)*1000:.1f} ms")

# --- Any vs basic-pitch predict() on the same wav
from basic_pitch.inference import predict
from basic_pitch import ICASSP_2022_MODEL_PATH
_, _, ev = predict(str(W / "theme2015.wav"), ICASSP_2022_MODEL_PATH)
bp = [(e[0], e[1], e[2], e[3]) for e in ev]; an = json.load(open(W / "any.json"))
f1, p, r = onset_f1([(s, m) for s, e, m, a in bp], [(x["start"], x["midi"]) for x in an])
print(f"any: basic-pitch {len(bp)} notes, rust {len(an)}; onset+pitch F1@50ms {f1:.3f} (P {p:.3f} R {r:.3f})")
# the same, with BP's own posteriors fed through the Rust decoder is covered by the Rust unit test
