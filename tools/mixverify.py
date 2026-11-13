#!/usr/bin/env python3
"""mixverify — a mix change stays only if two independent instruments agree.

One instrument is not an argument. The bus (`mix_masking`, live in the DAW)
lags 50-100 ms and sees whatever the session happens to be doing; the offline
render (`sdsp-chain` → `mixcheck.profile`) is deterministic and sees nothing
the DAW adds. Each has a blind spot the other does not, so a change is kept
only when both move the same way on the question asked.

    mixverify before.toml after.toml --question masking --labels before,after
    mixverify before.toml after.toml --question balance --track "chord" \\
              --labels pre,post --section 62 78 --ref ref.wav --ref-start 55 --ref-end 71
    mixverify before.toml after.toml --question loudness --labels a,b --json

Instrument 1, offline: both TOMLs are rendered with sdsp-chain (same inputs),
the renders are profiled with mixcheck (section-limited), and for `masking`
and `balance` the stems the question names are rendered too (every other
track muted, no master chain), so a band can be read on one part.
Instrument 2, bus: the two labelled `mix_masking` runs in
~/.superduper-dsp/mix-log.jsonl (clash shares, buried, per-track levels).

Questions, and what each instrument reads for them:

  masking    bus: the pair's clash share (lower is better)
             render: the pair's separation in the clash band on the stems,
             |E_a − E_b| in dB (wider is better: the bus calls "within 6 dB"
             a clash, so this is the same question asked offline)
  balance    bus: the track's level;  render: the track's stem RMS
  loudness   bus: the Master level;   render: integrated LUFS of the mix
  mono       render: the worst per-band mono loss; bus: nothing — the bus does
             not log a mono measure (mix_phase is not written to the log), so
             the verdict stops at UNKNOWN by the rule, and the numbers print

Thresholds (a move smaller than this is "no change", not evidence):
  LEVEL_DB  1.0   LUFS / LU, track and master levels
  BAND_DB   1.5   any band, stem separation
  SHARE     0.10  clash share (10 points of the time)

Verdict:
  KEEP     both instruments moved the same way by at least their threshold
           and no guard tripped
  UNDO     the instruments disagree (one moved, the other did not, or they
           moved opposite ways), both got worse, or a guard tripped: a band
           outside the question moved > 1.5 dB, LUFS moved > 1 LU, mono loss
           worsened > 1.5 dB in a band, another clash grew or appeared ≥ 10 pt
  UNKNOWN  an instrument is missing (no labels, no log, no stems, no chain
           binary) or neither instrument saw a change. Never KEEP on one.
"""

import argparse
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import numpy as np
import tomllib

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mixcheck

LEVEL_DB = 1.0
BAND_DB = 1.5
SHARE = 0.10
QUESTIONS = ("masking", "balance", "loudness", "mono")
DEFAULT_LOG = Path.home() / ".superduper-dsp" / "mix-log.jsonl"
HEADER = re.compile(r"^\s*\[\[\s*(track|master)\s*\]\]\s*(#.*)?$")
MUTE_LINE = re.compile(r"^\s*mute\s*=")


class VerifyError(Exception):
    pass


# ---------------------------------------------------------------------------
# sdsp-chain
# ---------------------------------------------------------------------------

def find_chain(explicit=None):
    """The renderer: --chain, $SDSP_CHAIN, $CARGO_TARGET_DIR, ./target, PATH."""
    root = Path(__file__).resolve().parent.parent
    cands = [explicit, os.environ.get("SDSP_CHAIN")]
    if os.environ.get("CARGO_TARGET_DIR"):
        cands.append(Path(os.environ["CARGO_TARGET_DIR"]) / "release" / "sdsp-chain")
    cands += [root / "target" / "release" / "sdsp-chain",
              Path.home() / ".cargo-target" / "release" / "sdsp-chain",
              shutil.which("sdsp-chain")]
    for c in cands:
        if c and Path(c).is_file() and os.access(c, os.X_OK):
            return Path(c)
    return None


def split_toml(text):
    """(preamble, [track block], [master block]) as lists of lines. A block
    runs from its `[[track]]`/`[[master]]` header to the next one; the
    `[[track.stage]]` sub-tables stay inside their track."""
    pre, tracks, masters, cur, kind = [], [], [], None, None
    for line in text.splitlines():
        m = HEADER.match(line)
        if m:
            if cur is not None:
                (tracks if kind == "track" else masters).append(cur)
            cur, kind = [line], m.group(1)
        elif cur is None:
            pre.append(line)
        else:
            cur.append(line)
    if cur is not None:
        (tracks if kind == "track" else masters).append(cur)
    return pre, tracks, masters


def stem_toml(text, keep):
    """The same mix with every track but `keep` (index) muted and no master
    chain: what that one part contributes, as the chain renders it. Muted
    tracks still render as keys, so ducks and sidechains survive."""
    pre, tracks, _ = split_toml(text)
    out = list(pre)
    for i, block in enumerate(tracks):
        head, body = block[0], [l for l in block[1:] if not MUTE_LINE.match(l)]
        out.append(head)
        if i != keep:
            out.append("mute = true")
        out.extend(body)
    return "\n".join(out) + "\n"


def track_names(cfg):
    return [t.get("name") or f"track {i + 1}" for i, t in enumerate(cfg.get("track", []))]


def render(chain, toml_path, out_wav, text=None, cli_input=None):
    """Run sdsp-chain on `toml_path` (or on `text` written next to it) with
    cwd at the TOML's directory, so relative `input` paths resolve."""
    toml_path = Path(toml_path).resolve()
    cfg_file = toml_path
    if text is not None:
        cfg_file = Path(out_wav).with_suffix(".toml")
        cfg_file.write_text(text)
    cmd = [str(chain), str(cfg_file), str(cli_input or "unused.wav"), str(out_wav)]
    r = subprocess.run(cmd, cwd=toml_path.parent, capture_output=True, text=True, check=False)
    if r.returncode != 0 or not Path(out_wav).is_file():
        raise VerifyError(f"sdsp-chain failed on {cfg_file.name}:\n{r.stdout[-800:]}\n{r.stderr[-800:]}")
    return out_wav


# ---------------------------------------------------------------------------
# Measurements
# ---------------------------------------------------------------------------

def band_db(l, r, sr, lo, hi, n_fft=4096):
    """Absolute energy (dB) of the mono fold in [lo, hi) Hz, mean per frame."""
    mono = (np.asarray(l) + np.asarray(r)) * 0.5
    n = (len(mono) // n_fft) * n_fft
    if n == 0:
        return -120.0
    spec = np.fft.rfft(mono[:n].reshape(-1, n_fft) * np.hanning(n_fft), axis=1)
    hz = np.fft.rfftfreq(n_fft, 1 / sr)
    m = (hz >= lo) & (hz < hi)
    return 10 * math.log10((np.abs(spec[:, m]) ** 2).sum() / (n / n_fft) + 1e-20)


def stem_reading(path, section, band):
    l, r, sr = mixcheck.load(path, *section)
    l, r = np.asarray(l), np.asarray(r)
    mono = (l + r) * 0.5
    out = {"rms_db": 10 * math.log10(float((mono * mono).mean()) + 1e-20)}
    if band:
        out["band_db"] = band_db(l, r, sr, *band)
    return out


def render_side(chain, toml_path, tmp, tag, section, stems, band, cli_input=None):
    """Instrument 1 for one TOML: the mix profile and the named stems."""
    toml_path = Path(toml_path)
    text = toml_path.read_text()
    names = track_names(tomllib.loads(text))
    wav_inputs = [t["input"] for t in tomllib.loads(text).get("track", []) if t.get("input")]
    placeholder = cli_input or (wav_inputs[0] if wav_inputs else None)
    mix = render(chain, toml_path, tmp / f"{tag}-mix.wav", cli_input=placeholder)
    side = {"toml": str(toml_path), "tracks": names, "mix": mixcheck.profile(mix, *section), "stems": {}}
    for name in stems:
        if name not in names:
            continue
        i = names.index(name)
        wav = render(chain, toml_path, tmp / f"{tag}-stem{i}.wav", text=stem_toml(text, i),
                     cli_input=placeholder)
        side["stems"][name] = stem_reading(wav, section, band)
    return side


def load_run(log_path, label):
    """The latest run with this label in mix-log.jsonl, or None."""
    p = Path(log_path)
    if not p.is_file():
        return None
    runs = []
    for line in p.read_text().splitlines():
        try:
            runs.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    hits = [r for r in runs if r.get("label") == label]
    return hits[-1] if hits else None


def clash_key(c):
    return (frozenset((c["a"], c["b"])), round(c["low_hz"]), round(c["high_hz"]))


def pair_name(c):
    return f"{c['a']} × {c['b']}"


def levels_of(run):
    return {x["track"]: x["rms_db"] for x in run.get("levels", []) if x.get("track")}


def buried_key(b):
    return (b["quiet"], b["under"], round(b["low_hz"]))


def buried_share(run, key):
    return next((x["share"] for x in run.get("buried", []) if buried_key(x) == key), 0.0)


def match_name(query, candidates):
    """The one candidate equal to, containing or contained in `query`
    (case-insensitive); None when there is no match or it is ambiguous."""
    q = query.casefold().strip()
    exact = [c for c in candidates if c.casefold().strip() == q]
    if len(exact) == 1:
        return exact[0]
    loose = [c for c in candidates if q in c.casefold() or c.casefold() in q]
    return loose[0] if len(loose) == 1 else None


# ---------------------------------------------------------------------------
# The questions: each instrument answers with a signed movement where
# positive means "the thing asked about went up" (separation, level, LUFS,
# mono headroom). Missing evidence is None, with a reason.
# ---------------------------------------------------------------------------

def reading(before, after, threshold, unit, what):
    delta = after - before
    return {"what": what, "before": before, "after": after, "delta": delta,
            "threshold": threshold, "unit": unit,
            "move": 1 if delta >= threshold else -1 if delta <= -threshold else 0}


def missing(reason):
    return {"missing": reason}


def choose_pair(args, b_before):
    """The clash the question is about: --pair (with --band or the bus
    band), else the worst clash in the before run."""
    if args.pair:
        a, b = [s.strip() for s in args.pair.split(",", 1)]
        if b_before:
            for c in sorted(b_before.get("clashes", []), key=lambda c: -c["share"]):
                names = b_before.get("tracks", []) + [c["a"], c["b"]]
                if {match_name(a, names), match_name(b, names)} == {c["a"], c["b"]}:
                    return c
        if args.band:
            return {"a": a, "b": b, "low_hz": args.band[0], "high_hz": args.band[1], "share": 0.0}
        return {"a": a, "b": b, "low_hz": None, "high_hz": None, "share": 0.0}
    if b_before and b_before.get("clashes"):
        return max(b_before["clashes"], key=lambda c: c["share"])
    return None


def ask_masking(args, r1, r2, b1, b2, pair):
    if pair is None:
        return missing("no clash pair: no clashes in the before run and no --pair"), \
            missing("no clash pair to read")
    band = (pair["low_hz"], pair["high_hz"])
    # instrument 2: the pair's share, same band, in each run (absent = 0)
    if b1 is None or b2 is None:
        bus = missing("bus runs not loaded")
    else:
        def share(run):
            hits = [c for c in run.get("clashes", []) if clash_key(c) == clash_key(pair)]
            return hits[0]["share"] if hits else 0.0
        s1, s2 = share(b1), share(b2)
        bus = reading(-s1, -s2, SHARE, "share", f"clash share {pair_name(pair)}")
        bus.update(before=s1, after=s2, delta=s2 - s1)
    # instrument 1: separation on the two stems
    if band[0] is None:
        return missing("no band for the pair (give --band LO HI or --labels)"), bus
    if r1 is None or r2 is None:
        return missing("renders not available"), bus
    a, b = (r1["stem_names"].get(pair["a"]), r1["stem_names"].get(pair["b"]))
    if not a or not b:
        return missing(f"no stem for {pair_name(pair)}: TOML tracks are {r1['tracks']}"), bus
    sep1 = abs(r1["stems"][a]["band_db"] - r1["stems"][b]["band_db"])
    sep2 = abs(r2["stems"][a]["band_db"] - r2["stems"][b]["band_db"])
    rend = reading(sep1, sep2, BAND_DB, "dB", f"separation {a} / {b} at {band[0]:.0f}-{band[1]:.0f} Hz")
    return rend, bus


def ask_balance(args, r1, r2, b1, b2, track):
    if track is None:
        return missing("no track: give --track or --labels"), missing("no track")
    if b1 is None or b2 is None:
        bus = missing("bus runs not loaded")
    else:
        l1, l2 = levels_of(b1), levels_of(b2)
        t = match_name(track, list(l1) + list(l2))
        bus = reading(l1[t], l2[t], LEVEL_DB, "dB", f"bus level {t}") \
            if t and t in l1 and t in l2 else missing(f"track {track!r} not in both runs")
    if r1 is None or r2 is None:
        return missing("renders not available"), bus
    t = r1["stem_names"].get(track)
    if not t:
        return missing(f"no stem for {track!r}: TOML tracks are {r1['tracks']}"), bus
    return reading(r1["stems"][t]["rms_db"], r2["stems"][t]["rms_db"], LEVEL_DB, "dB", f"stem RMS {t}"), bus


def ask_loudness(args, r1, r2, b1, b2):
    if b1 is None or b2 is None:
        bus = missing("bus runs not loaded")
    else:
        l1, l2 = levels_of(b1), levels_of(b2)
        if "Master" in l1 and "Master" in l2:
            bus = reading(l1["Master"], l2["Master"], LEVEL_DB, "dB", "bus Master level")
        else:
            bus = missing("no 'Master' level in both runs")
    if r1 is None or r2 is None:
        return missing("renders not available"), bus
    a, b = r1["mix"].get("lufs_integrated"), r2["mix"].get("lufs_integrated")
    if a is None or b is None or not (math.isfinite(a) and math.isfinite(b)):
        return missing("LUFS unreadable (silence?)"), bus
    return reading(a, b, LEVEL_DB, "LU", "integrated LUFS"), bus


def ask_mono(args, r1, r2, b1, b2):
    bus = missing("the bus logs no mono measure (mix_phase is not written to mix-log.jsonl)")
    if r1 is None or r2 is None:
        return missing("renders not available"), bus
    m1, m2 = r1["mix"].get("mono_loss_db") or {}, r2["mix"].get("mono_loss_db") or {}
    if not m1 or not m2:
        return missing("mono loss unreadable"), bus
    return reading(min(m1.values()), min(m2.values()), BAND_DB, "dB", "worst-band mono loss"), bus


# ---------------------------------------------------------------------------
# Guards: what must not get worse while the question is answered
# ---------------------------------------------------------------------------

def bands_touching(band):
    if not band or band[0] is None:
        return set()
    return {name for lo, hi, name in mixcheck.BANDS if lo < band[1] and hi > band[0]}


def guards(question, r1, r2, b1, b2, pair, track):
    """Each tripped guard as (instrument, text). Exemptions are the metrics
    the question itself moves: a fader move changes LUFS and band balance,
    a cut changes its own band, a loudness move changes LUFS."""
    out = []
    if r1 and r2:
        p1, p2 = r1["mix"], r2["mix"]
        if question in ("masking", "mono"):
            a, b = p1.get("lufs_integrated"), p2.get("lufs_integrated")
            if a is not None and b is not None and abs(b - a) > LEVEL_DB:
                out.append(("render", f"integrated LUFS moved {b - a:+.1f} LU (> {LEVEL_DB})"))
        if question != "balance":
            exempt = bands_touching((pair["low_hz"], pair["high_hz"])) if question == "masking" and pair else set()
            for name in p1["bands_db"]:
                d = p2["bands_db"][name] - p1["bands_db"][name]
                if name not in exempt and abs(d) > BAND_DB:
                    out.append(("render", f"band {name} moved {d:+.1f} dB (> {BAND_DB}) outside the question"))
        if question != "mono":
            m1, m2 = p1.get("mono_loss_db") or {}, p2.get("mono_loss_db") or {}
            for name in m1:
                if name in m2 and m1[name] - m2[name] > BAND_DB:
                    out.append(("render", f"mono loss in {name} worsened {m2[name] - m1[name]:+.1f} dB (> {BAND_DB})"))
    if b1 and b2:
        c1 = {clash_key(c): c for c in b1.get("clashes", [])}
        for c in b2.get("clashes", []):
            k = clash_key(c)
            if pair and k == clash_key(pair):
                continue
            grew = c["share"] - (c1[k]["share"] if k in c1 else 0.0)
            if grew >= SHARE:
                verb = "grew" if k in c1 else "appeared"
                out.append(("bus", (f"clash {pair_name(c)} at {c['low_hz']:.0f}-{c['high_hz']:.0f} Hz "
                                    f"{verb} {grew * 100:+.0f} pt (≥ {SHARE * 100:.0f})")))
        if question == "balance" and track:
            l1, l2 = levels_of(b1), levels_of(b2)
            t = match_name(track, list(l1))
            for name in l1:
                if name in l2 and name not in (t, "Master") and abs(l2[name] - l1[name]) > LEVEL_DB:
                    out.append(("bus", f"level of {name} moved {l2[name] - l1[name]:+.1f} dB (> {LEVEL_DB}), not the track asked about"))
    return out


def describe(inst, r):
    """One line per instrument: what it read, by how much, against what."""
    if r["unit"] == "share":
        return f"{inst}: {r['what']} {r['delta'] * 100:+.0f} pt (threshold {r['threshold'] * 100:.0f} pt)"
    return f"{inst}: {r['what']} {r['delta']:+.1f} {r['unit']} (threshold {r['threshold']} {r['unit']})"


def verdict(question, rend, bus, tripped, expect=None):
    """The rule, in one place."""
    for inst, r in (("render", rend), ("bus", bus)):
        if "missing" in r:
            return "UNKNOWN", [f"{inst} instrument missing: {r['missing']}; one instrument is not an argument"]
    reasons = [describe("render", rend), describe("bus", bus)]
    m1, m2 = rend["move"], bus["move"]
    if tripped:
        return "UNDO", reasons + [f"guard: {inst}: {text}" for inst, text in tripped]
    if m1 == 0 and m2 == 0:
        return "UNKNOWN", reasons + ["no change on either instrument (both under threshold)"]
    if m1 != m2:
        return "UNDO", reasons + ["the instruments disagree"]
    if question in ("masking", "mono"):
        return ("KEEP", reasons + ["both say better"]) if m1 > 0 else ("UNDO", reasons + ["both say worse"])
    want = {"up": 1, "down": -1}.get(expect)
    if want and m1 != want:
        return "UNDO", reasons + [f"both moved {'up' if m1 > 0 else 'down'}, you expected {expect}"]
    return "KEEP", reasons + [f"both moved {'up' if m1 > 0 else 'down'}"]


# ---------------------------------------------------------------------------
# Output
# ---------------------------------------------------------------------------

def fmt(v, w=8, prec=1, sign=False):
    if v is None or (isinstance(v, float) and not math.isfinite(v)):
        return "—".rjust(w)
    return f"{v:{'+' if sign else ''}{w}.{prec}f}"


def print_render(r1, r2, ref, pair, question):
    p1, p2 = r1["mix"], r2["mix"]
    print("\ninstrument 1 · offline render (sdsp-chain → mixcheck.profile)")
    head = f"  {'metric':28s}{'before':>9s}{'after':>9s}{'delta':>9s}"
    if ref:
        head += f"{'vs ref':>9s}{'→ ref':>9s}"
    print(head)

    def row(label, a, b, unit="", ra=None, rb=None, prec=1):
        line = f"  {label:28s}{fmt(a, 9, prec)}{fmt(b, 9, prec)}{fmt(None if a is None or b is None else b - a, 8, prec, True)} {unit}"
        if ref and ra is not None:
            line += f"{fmt(ra, 8, prec, True)}{fmt(rb, 9, prec, True)}"
        print(line)

    rl = ref.get("lufs_integrated") if ref else None
    row("LUFS integrated", p1.get("lufs_integrated"), p2.get("lufs_integrated"), "LU",
        *((p1["lufs_integrated"] - rl, p2["lufs_integrated"] - rl) if rl is not None else (None, None)))
    row("LUFS short-term max", p1.get("lufs_short_max"), p2.get("lufs_short_max"), "LU")
    row("crest", p1["crest_db"], p2["crest_db"], "dB")
    row("correlation", p1["correlation"], p2["correlation"], "", prec=2)
    row("side/mid < 120 Hz", p1.get("side_mid_low_db"), p2.get("side_mid_low_db"), "dB")
    exempt = bands_touching((pair["low_hz"], pair["high_hz"])) if question == "masking" and pair and pair["low_hz"] else set()
    for lo, hi, name in mixcheck.BANDS:
        tag = f"band {name:9s}{lo:5d}-{hi:<5d}" + ("*" if name in exempt else "")
        ra = rb = None
        if ref:
            ra, rb = p1["bands_db"][name] - ref["bands_db"][name], p2["bands_db"][name] - ref["bands_db"][name]
        row(tag, p1["bands_db"][name], p2["bands_db"][name], "dB", ra, rb)
    m1, m2 = p1.get("mono_loss_db") or {}, p2.get("mono_loss_db") or {}
    for name in m1:
        if name in m2 and (min(m1[name], m2[name]) < -1 or abs(m2[name] - m1[name]) > 0.5):
            row(f"mono loss {name}", m1[name], m2[name], "dB")
    if exempt:
        print("  (* the clash band: exempt from the band guard)")
    if ref:
        d1 = sum(abs(p1["bands_db"][n] - ref["bands_db"][n]) for _, _, n in mixcheck.BANDS)
        d2 = sum(abs(p2["bands_db"][n] - ref["bands_db"][n]) for _, _, n in mixcheck.BANDS)
        print(f"  distance to reference Σ|Δband|  {d1:6.1f} → {d2:6.1f} dB  (shown, not judged)")
    for name in r1["stems"]:
        s1, s2 = r1["stems"][name], r2["stems"].get(name)
        if not s2:
            continue
        row(f"stem {name[:23]}", s1["rms_db"], s2["rms_db"], "dB RMS")
        if "band_db" in s1:
            row(f"  @ {pair['low_hz']:.0f}-{pair['high_hz']:.0f} Hz", s1["band_db"], s2["band_db"], "dB")


def print_bus(b1, b2, labels, pair):
    print(f"\ninstrument 2 · bus (mix-log.jsonl: {labels[0]!r} → {labels[1]!r})")
    c1 = {clash_key(c): c for c in b1.get("clashes", [])}
    c2 = {clash_key(c): c for c in b2.get("clashes", [])}
    print(f"  {'clash':36s}{'band':>14s}{'before':>8s}{'after':>8s}{'delta':>8s}")
    for k in sorted(set(c1) | set(c2), key=lambda k: -(c1.get(k, c2.get(k))["share"])):
        c = c1.get(k) or c2[k]
        s1, s2 = (c1[k]["share"] if k in c1 else 0.0), (c2[k]["share"] if k in c2 else 0.0)
        mark = "  ← the question" if pair and k == clash_key(pair) else ""
        print(f"  {pair_name(c)[:36]:36s}{c['low_hz']:6.0f}-{c['high_hz']:<7.0f}{s1 * 100:7.0f} %{s2 * 100:7.0f} %{(s2 - s1) * 100:+7.0f} pt{mark}")
    if not c1 and not c2:
        print("  (no clashes in either run)")
    bur = {buried_key(b): b for b in b1.get("buried", []) + b2.get("buried", [])}
    if bur:
        print(f"  {'buried':36s}{'band':>14s}{'before':>8s}{'after':>8s}")
        for key, b in bur.items():
            s1, s2 = buried_share(b1, key), buried_share(b2, key)
            print(f"  {(b['quiet'] + ' under ' + b['under'])[:36]:36s}{b['low_hz']:6.0f}-{b['high_hz']:<7.0f}{s1 * 100:7.0f} %{s2 * 100:7.0f} %")
    l1, l2 = levels_of(b1), levels_of(b2)
    print(f"  {'level':36s}{'':>14s}{'before':>8s}{'after':>8s}{'delta':>8s}")
    for name in sorted(set(l1) | set(l2)):
        a, b = l1.get(name), l2.get(name)
        if a is not None and b is not None and max(a, b) > -100:
            print(f"  {name[:36]:36s}{'':>14s}{a:8.1f}{b:8.1f}{b - a:+8.1f} dB")


def finite(v):
    if isinstance(v, float):
        return v if math.isfinite(v) else None
    if isinstance(v, dict):
        return {k: finite(x) for k, x in v.items()}
    if isinstance(v, (list, tuple)):
        return [finite(x) for x in v]
    return v


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

def parse_args(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("before", help="sdsp-chain TOML before the change")
    ap.add_argument("after", help="sdsp-chain TOML after the change (same inputs)")
    ap.add_argument("--question", choices=QUESTIONS, default="masking")
    ap.add_argument("--labels", help="before,after labels of two mix_masking runs in the mix log")
    ap.add_argument("--section", nargs=2, type=float, metavar=("A", "B"), help="seconds: profile the renders in [A, B)")
    ap.add_argument("--ref", help="reference WAV: band deltas against it are shown, not judged")
    ap.add_argument("--ref-start", type=float)
    ap.add_argument("--ref-end", type=float)
    ap.add_argument("--pair", help="masking: the two tracks, 'A,B' (default: the worst clash in the before run)")
    ap.add_argument("--band", nargs=2, type=float, metavar=("LO", "HI"), help="masking: the clash band in Hz when there is no bus run")
    ap.add_argument("--track", help="balance: the track asked about (default: the bus track that moved most)")
    ap.add_argument("--expect", choices=("up", "down"), help="balance/loudness: the direction you meant")
    ap.add_argument("--input", help="input WAV for single-chain TOMLs ([[stage]] only)")
    ap.add_argument("--mix-log", default=str(DEFAULT_LOG))
    ap.add_argument("--chain", help="sdsp-chain binary (default: $SDSP_CHAIN, $CARGO_TARGET_DIR, ./target)")
    ap.add_argument("--keep", help="directory to keep the renders in (default: a temp dir, deleted)")
    ap.add_argument("--json", action="store_true")
    return ap.parse_args(argv)


def run(args):
    """Everything but printing: returns the result dict (also the --json)."""
    section = tuple(args.section) if args.section else (None, None)
    labels = [s.strip() for s in args.labels.split(",", 1)] if args.labels else None
    b1 = b2 = None
    bus_note = "no --labels"
    if labels and len(labels) == 2:
        b1, b2 = load_run(args.mix_log, labels[0]), load_run(args.mix_log, labels[1])
        bus_note = None if b1 and b2 else f"label(s) not in {args.mix_log}: " + ", ".join(
            l for l, r in zip(labels, (b1, b2)) if not r)
    pair = choose_pair(args, b1) if args.question == "masking" else None
    track = args.track
    if args.question == "balance" and not track and b1 and b2:
        l1, l2 = levels_of(b1), levels_of(b2)
        moved = {n: abs(l2[n] - l1[n]) for n in l1 if n in l2 and n != "Master" and max(l1[n], l2[n]) > -100}
        track = max(moved, key=moved.get) if moved else None

    r1 = r2 = None
    render_note = None
    chain = find_chain(args.chain)
    if not chain:
        render_note = "sdsp-chain not found: build with `cargo build --release -p sdsp-chain` or pass --chain"
    else:
        cfg1 = tomllib.loads(Path(args.before).read_text())
        names = track_names(cfg1)
        wanted = {}
        if args.question == "masking" and pair:
            for n in (pair["a"], pair["b"]):
                m = match_name(n, names)
                if m:
                    wanted[n] = m
        elif args.question == "balance" and track:
            m = match_name(track, names)
            if m:
                wanted[track] = m
        band = (pair["low_hz"], pair["high_hz"]) if pair and pair.get("low_hz") is not None else None
        tmp = Path(args.keep) if args.keep else Path(tempfile.mkdtemp(prefix="mixverify-"))
        tmp.mkdir(parents=True, exist_ok=True)
        try:
            r1 = render_side(chain, args.before, tmp, "before", section, set(wanted.values()), band, args.input)
            r2 = render_side(chain, args.after, tmp, "after", section, set(wanted.values()), band, args.input)
            r1["stem_names"] = r2["stem_names"] = wanted
        except VerifyError as e:
            render_note = str(e)
            r1 = r2 = None
        finally:
            if not args.keep:
                shutil.rmtree(tmp, ignore_errors=True)
    ref = mixcheck.profile(args.ref, args.ref_start, args.ref_end) if args.ref else None

    ask = {"masking": lambda: ask_masking(args, r1, r2, b1, b2, pair),
           "balance": lambda: ask_balance(args, r1, r2, b1, b2, track),
           "loudness": lambda: ask_loudness(args, r1, r2, b1, b2),
           "mono": lambda: ask_mono(args, r1, r2, b1, b2)}[args.question]
    rend, bus = ask()
    if "missing" in rend and render_note:
        rend = missing(render_note)
    if "missing" in bus and bus_note:
        bus = missing(bus_note)
    tripped = guards(args.question, r1, r2, b1, b2, pair, track)
    v, reasons = verdict(args.question, rend, bus, tripped, args.expect)
    return {"question": args.question, "before": args.before, "after": args.after, "labels": labels,
            "section": list(section), "pair": pair, "track": track, "verdict": v, "reasons": reasons,
            "render": {"reading": rend, "before": r1, "after": r2, "note": render_note},
            "bus": {"reading": bus, "before": b1, "after": b2, "note": bus_note},
            "guards": [{"instrument": i, "text": t} for i, t in tripped],
            "thresholds": {"level_db": LEVEL_DB, "band_db": BAND_DB, "share": SHARE},
            "reference": ref}


def main(argv=None):
    args = parse_args(argv)
    res = run(args)
    if args.json:
        print(json.dumps(finite(res), indent=1, ensure_ascii=False))
        return res["verdict"]
    sec = f"   section {res['section'][0]}–{res['section'][1]} s" if res["section"][0] is not None else ""
    print(f"mixverify   {Path(args.before).name} → {Path(args.after).name}   question: {args.question}{sec}")
    pair = res["pair"]
    if args.question == "masking" and pair:
        band = f" at {pair['low_hz']:.0f}-{pair['high_hz']:.0f} Hz" if pair.get("low_hz") is not None else ""
        print(f"pair: {pair_name(pair)}{band}" + ("" if args.pair else "   (worst clash in the before run; name one with --pair)"))
    if args.question == "balance":
        print(f"track: {res['track']}")
    if res["render"]["before"]:
        print_render(res["render"]["before"], res["render"]["after"], res["reference"], pair, args.question)
    else:
        print(f"\ninstrument 1 · offline render: MISSING — {res['render']['note']}")
    if res["bus"]["before"] and res["bus"]["after"]:
        print_bus(res["bus"]["before"], res["bus"]["after"], res["labels"], pair)
    else:
        print(f"\ninstrument 2 · bus: MISSING — {res['bus']['note']}")
    print(f"\nVERDICT: {res['verdict']}")
    for r in res["reasons"]:
        print(f"  {r}")
    return res["verdict"]


if __name__ == "__main__":
    sys.exit({"KEEP": 0, "UNDO": 1, "UNKNOWN": 2}[main()])
