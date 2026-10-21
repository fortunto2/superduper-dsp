#!/usr/bin/env python3
"""mixopt — move a few whitelisted plugin parameters until a measured loss stops falling.

The renderer (sdsp-chain) is deterministic and a render costs ~0.1 s, so the
mix is a function we can minimise directly: no population, no mutation, no
"evolution" — a bounded Nelder–Mead (or Powell) over the handful of parameters
the recipe allows, each step measured, every evaluation logged.

    mixopt base.toml --vary "gtr.eq.Mid Gain:-12:0" "gtr.eq.Mid Freq:150:600" \\
           --loss "ref:ref.wav,masking,loudness" --iters 60 --out best.toml

--vary   "<track>.<stage>.<param>:<lo>:<hi>"  stage = plugin key, `eq#2` = the
         second eq on that track; "<track>.gain_db:lo:hi" moves the fader;
         "master.<stage>.<param>" addresses the [[master]] chain. Bounds are
         clipped to the plugin's own range (`sdsp-chain --params <plugin>`).
--loss   comma list, each term optionally `=weight`:
           ref:<wav|mixcheck.json>  band-balance distance to the reference,
                                    dB RMS over mixcheck's bands, level-free
           loudness                 |LUFS gap| to the reference (needs ref:)
           masking                  Σ over stem pairs of Σ over 32 bus bands of
                                    the share of frames where both are > −50 dB
                                    and within 6 dB (synth-core/src/mixmap.rs)
           level                    a stem that drops ≥ 2 dB against the base
                                    pays the excess in dB (quieter ≠ better)
--iters  evaluations; 0 = measure the base and stop.  --resume continues from
         the best point in the JSONL log.  Every evaluation is one line of
         <out>.jsonl: the parameter diff against the base and every loss term.

Runs from the tools/ directory or by path: `python3 tools/mixopt.py …`.
"""
# AI-NOTE: masking mirrors synth-core/src/mixmap.rs + gui.rs::scope_spectrum (Hann 4096,
# 16/(3N²) Parseval, 32 log bands); stems render one TOML per track, inputs absolutised
# because sdsp-chain resolves paths against the CWD, not the config.
import argparse
import contextlib
import copy
import json
import math
import os
import re
import shutil
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import tomllib
from scipy.optimize import minimize

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mixcheck  # same directory: band table + BS.1770 LUFS

BUS_BANDS = 32
LOUD_DB = -50.0
CLOSE_DB = 6.0
LEVEL_FREE_DB = 2.0
N_FFT = 4096
HOP = 2048


# ---------------------------------------------------------------------------
# Renderer
# ---------------------------------------------------------------------------

def find_chain():
    """The sdsp-chain binary, or None. Order: $SDSP_CHAIN, $CARGO_TARGET_DIR,
    ~/.cargo-target, <repo>/target, PATH."""
    repo = Path(__file__).resolve().parents[1]
    cands = [os.environ.get("SDSP_CHAIN")]
    if os.environ.get("CARGO_TARGET_DIR"):
        cands.append(Path(os.environ["CARGO_TARGET_DIR"]) / "release" / "sdsp-chain")
    cands += [Path.home() / ".cargo-target/release/sdsp-chain", repo / "target/release/sdsp-chain",
              shutil.which("sdsp-chain")]
    for c in cands:
        if c and Path(c).is_file() and os.access(c, os.X_OK):
            return str(c)
    return None


_PARAM_ROW = re.compile(r"^\s*(\d+)\s+(.+?)\s+(-?[\d.]+)\s+(-?[\d.]+)\s+(-?[\d.]+)\s*(\S*)\s*$")
_tables = {}


def param_table(chain, plugin):
    """`sdsp-chain --params <plugin>` as a list of dicts, cached."""
    if plugin not in _tables:
        r = subprocess.run([chain, "--params", plugin], capture_output=True, text=True, check=False)
        if r.returncode != 0:
            raise SystemExit(f"sdsp-chain --params {plugin}: {r.stderr.strip() or r.stdout.strip()}")
        rows = []
        for line in r.stdout.splitlines():
            m = _PARAM_ROW.match(line)
            if m and not line.lstrip().startswith("id "):
                rows.append({"id": int(m[1]), "name": m[2].strip(), "min": float(m[3]),
                             "max": float(m[4]), "default": float(m[5]), "unit": m[6]})
        if not rows:
            raise SystemExit(f"could not parse `sdsp-chain --params {plugin}`")
        _tables[plugin] = rows
    return _tables[plugin]


def render(chain, cfg, toml_path, out_wav):
    """Write cfg as TOML, render it, return the WAV path. Raises on failure."""
    cfg = dict(cfg)
    cfg["out"] = str(out_wav)
    Path(toml_path).write_text(dump_toml(cfg))
    r = subprocess.run([chain, str(toml_path)], capture_output=True, text=True, check=False)
    if r.returncode != 0:
        raise RuntimeError(f"sdsp-chain failed on {toml_path}:\n{r.stderr.strip() or r.stdout.strip()}")
    return Path(out_wav)


# ---------------------------------------------------------------------------
# TOML out (tomllib reads; this writes the subset sdsp-chain uses)
# ---------------------------------------------------------------------------

def _key(k):
    return k if re.fullmatch(r"[A-Za-z0-9_-]+", k) else json.dumps(k, ensure_ascii=False)


def _fmt(v):
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, int):
        return str(v)
    if isinstance(v, float):
        return repr(v) if math.isfinite(v) else ("inf" if v > 0 else "-inf" if v < 0 else "nan")
    if isinstance(v, str):
        return json.dumps(v, ensure_ascii=False)
    if isinstance(v, list):
        return "[" + ", ".join(_fmt(x) for x in v) + "]"
    if isinstance(v, dict):
        return "{}" if not v else "{ " + ", ".join(f"{_key(k)} = {_fmt(x)}" for k, x in v.items()) + " }"
    raise TypeError(f"cannot write {type(v).__name__} to TOML")


def _dump(d, path, out):
    tables = []
    for k, v in d.items():
        if isinstance(v, list) and v and all(isinstance(x, dict) for x in v):
            tables.append((k, v))
        else:
            out.append(f"{_key(k)} = {_fmt(v)}")
    for k, items in tables:
        for item in items:
            out.append("")
            out.append("[[" + ".".join(_key(p) for p in path + [k]) + "]]")
            _dump(item, path + [k], out)


def dump_toml(cfg):
    out = []
    _dump(cfg, [], out)
    return "\n".join(out) + "\n"


# ---------------------------------------------------------------------------
# Audio in + meters
# ---------------------------------------------------------------------------

def read_wav(path):
    """(left, right, sr) as float arrays; 16/24/32-bit int and 32-bit float."""
    raw = Path(path).read_bytes()
    if raw[:4] != b"RIFF" or raw[8:12] != b"WAVE":
        raise ValueError(f"{path}: not a WAV file")
    i, fmt, ch, bits, sr, data = 12, 1, 2, 16, 48000, None
    while i + 8 <= len(raw):
        cid, size = raw[i:i + 4], int.from_bytes(raw[i + 4:i + 8], "little")
        body = raw[i + 8:i + 8 + size]
        if cid == b"fmt ":
            fmt = int.from_bytes(body[0:2], "little")
            ch = int.from_bytes(body[2:4], "little")
            sr = int.from_bytes(body[4:8], "little")
            bits = int.from_bytes(body[14:16], "little")
            if fmt == 0xFFFE and len(body) >= 26:  # extensible: sub-format code
                fmt = int.from_bytes(body[24:26], "little")
        elif cid == b"data":
            data = body
            break
        i += 8 + size + (size & 1)
    if data is None:
        raise ValueError(f"{path}: no data chunk")
    if fmt == 3 and bits == 32:
        x = np.frombuffer(data, dtype="<f4").astype(np.float64)
    elif fmt == 1 and bits == 16:
        x = np.frombuffer(data, dtype="<i2").astype(np.float64) / 32768.0
    elif fmt == 1 and bits == 32:
        x = np.frombuffer(data, dtype="<i4").astype(np.float64) / 2147483648.0
    elif fmt == 1 and bits == 24:
        b = np.frombuffer(data, dtype=np.uint8)
        b = b[: len(b) // 3 * 3].reshape(-1, 3).astype(np.int32)
        x = ((b[:, 0] | (b[:, 1] << 8) | (b[:, 2] << 16)) << 8 >> 8).astype(np.float64) / 8388608.0
    else:
        raise ValueError(f"{path}: unsupported WAV format {fmt}/{bits}-bit")
    n = len(x) // ch
    x = x[: n * ch].reshape(n, ch)
    # Contiguous copies: a strided column view makes scipy's lfilter 50-250× slower
    # (measured 2026-10-21: 1.3 s vs 5 ms on 4 s of audio).
    return np.ascontiguousarray(x[:, 0]), np.ascontiguousarray(x[:, 1] if ch > 1 else x[:, 0]), float(sr)


def _frames(mono, n=N_FFT, hop=HOP):
    """Hann-windowed one-sided power spectra, one row per frame."""
    x = np.asarray(mono, dtype=np.float64)
    if len(x) < n:
        x = np.pad(x, (0, n - len(x)))
    win = np.hanning(n)  # 0.5 − 0.5 cos(2πi/(n−1)), as scope_spectrum
    fr = np.lib.stride_tricks.sliding_window_view(x, n)[::hop] * win
    return np.abs(np.fft.rfft(fr, axis=1)) ** 2


def bus_band_edges():
    """32 log bands 20 Hz … 20 kHz (sdk bus::band_edges)."""
    return 20.0 * 1000.0 ** (np.arange(BUS_BANDS + 1) / BUS_BANDS)


def bus_bands(mono, sr, n=N_FFT, hop=HOP):
    """Per frame, the 32 bus bands in dB of band RMS — the exact normalisation
    the plugins publish (gui.rs::scope_spectrum): Parseval with a Hann window,
    rms² in a bin set = 16/(3N²)·Σ|X_k|², so a track's bands sum to its level."""
    pw = _frames(mono, n, hop)
    bin_hz = sr / n
    norm = 16.0 / (3.0 * n * n)
    edges = bus_band_edges()
    out = np.empty((pw.shape[0], BUS_BANDS))
    for b in range(BUS_BANDS):
        lo, hi = edges[b] / bin_hz, edges[b + 1] / bin_hz
        i0, i1 = math.ceil(lo), math.ceil(hi)
        if i1 > i0:
            p = pw[:, i0:min(i1, pw.shape[1])].sum(axis=1)
        else:  # narrower than a bin: that bin's power, shared by width
            k = round(lo)
            p = pw[:, k] * (hi - lo) if k < pw.shape[1] else np.zeros(pw.shape[0])
        out[:, b] = 10 * np.log10(np.maximum(p * norm, 1e-12))
    return out


def balance_bands(mono, sr):
    """mixcheck's bands (sub … top) as dB centred on their mean, so the vector
    is a shape and loudness stays its own term."""
    pw = _frames(mono, N_FFT, HOP).sum(axis=0)
    hz = np.fft.rfftfreq(N_FFT, 1 / sr)
    v = np.array([pw[(hz >= lo) & (hz < hi)].sum() for lo, hi, _ in mixcheck.BANDS])
    v = 10 * np.log10(np.maximum(v, 1e-20))
    return v - v.mean()


def masking_shares(stems):
    """{(a, b): 32-vector} — share of frames where both stems are > LOUD_DB in
    the band and within CLOSE_DB of each other (mixmap.rs, `MaskingTracker::mean`)."""
    names = sorted(stems)
    out = {}
    for i, a in enumerate(names):
        for b in names[i + 1:]:
            n = min(len(stems[a]), len(stems[b]))
            p, q = stems[a][:n], stems[b][:n]
            clash = (p > LOUD_DB) & (q > LOUD_DB) & (np.abs(p - q) < CLOSE_DB)
            out[(a, b)] = clash.mean(axis=0) if n else np.zeros(BUS_BANDS)
    return out


def masking_report(shares):
    """Pairs sorted worst first: worst band (Hz) and its share, and the total."""
    edges = bus_band_edges()
    rows = []
    for (a, b), s in shares.items():
        k = int(np.argmax(s))
        rows.append({"a": a, "b": b, "band": k, "hz": (round(edges[k]), round(edges[k + 1])),
                     "worst_share": float(s[k]), "total": float(s.sum())})
    return sorted(rows, key=lambda r: -r["total"])


def level_db(l, r):
    return 10 * math.log10(float(np.mean(l * l + r * r)) / 2 + 1e-20)


def avg_bands(l, r, sr):
    """Time-averaged bus bands (dB of mean power), as mixmap.py averages them."""
    b = bus_bands((l + r) * 0.5, sr)
    return 10 * np.log10(np.mean(10 ** (b / 10), axis=0) + 1e-12)


def broadband_drop(base, now, floor_db=-80.0):
    """How much quieter a stem got as a whole: the median per-band drop over the
    bands that carry energy in the base (mixmap.rs::sum_deficit_db strips the
    broadband part the same way). A fader move drops every band and shows in
    full; a narrow bell moves a few bands and the median stays near 0."""
    m = base > floor_db
    return float(np.median(base[m] - now[m])) if m.any() else 0.0


def cut(l, r, sr, span):
    if not span:
        return l, r
    i0, i1 = int(span[0] * sr), int(span[1] * sr) if span[1] is not None else len(l)
    if i1 <= i0 or i0 >= len(l):
        raise SystemExit(f"empty section {span[0]}..{span[1]} s (file is {len(l) / sr:.1f} s)")
    return l[i0:i1], r[i0:i1]


# ---------------------------------------------------------------------------
# Whitelist: which (track, stage, param) may move, and between what
# ---------------------------------------------------------------------------

@dataclass
class Target:
    key: str
    track: str          # track name or "master"
    stage: str | None   # plugin key, or None for a track field (gain_db, pan)
    nth: int            # which stage of that plugin on the track, 0-based
    param: str
    lo: float
    hi: float
    start: float = 0.0
    param_key: str = ""  # the key actually written into params (name or numeric id)

    def clip(self, v):
        return min(self.hi, max(self.lo, v))


def _stage_of(cfg, t):
    tracks = cfg.get("track", [])
    if t.track == "master":
        stages = cfg.get("master", [])
    else:
        tr = next((x for x in tracks if x.get("name") == t.track), None)
        if tr is None:
            raise SystemExit(f"--vary {t.key}: no track named '{t.track}' (have: "
                             + ", ".join(x.get("name", "?") for x in tracks) + ")")
        if t.stage is None:
            return tr
        stages = tr.get("stage", [])
    hits = [s for s in stages if s.get("plugin") == t.stage]
    if len(hits) <= t.nth:
        raise SystemExit(f"--vary {t.key}: '{t.track}' has {len(hits)} '{t.stage}' stage(s), "
                         f"wanted #{t.nth + 1}")
    return hits[t.nth]


def parse_vary(spec, cfg, chain):
    """'gtr.eq.Mid Gain:-12:0' → Target with start value and real bounds."""
    try:
        key, lo, hi = spec.rsplit(":", 2)
        lo, hi = float(lo), float(hi)
    except ValueError:
        raise SystemExit(f"--vary '{spec}': want <track>.<stage>.<param>:<lo>:<hi>")
    if hi <= lo:
        raise SystemExit(f"--vary '{spec}': hi must exceed lo")
    parts = key.split(".")
    if len(parts) == 2:
        t = Target(key, parts[0], None, 0, parts[1], lo, hi)
        if t.param not in ("gain_db", "pan"):
            raise SystemExit(f"--vary '{spec}': track fields are gain_db or pan")
        tr = _stage_of(cfg, t)
        t.start, t.param_key = float(tr.get(t.param, 0.0)), t.param
        return t
    if len(parts) < 3:
        raise SystemExit(f"--vary '{spec}': want <track>.<stage>.<param>")
    stage, nth = parts[-2], 0
    if "#" in stage:
        stage, n = stage.split("#", 1)
        nth = int(n) - 1
    t = Target(key, ".".join(parts[:-2]), stage, nth, parts[-1], lo, hi)
    st = _stage_of(cfg, t)
    rows = param_table(chain, t.stage)
    row = next((r for r in rows if r["name"] == t.param), None) or \
        next((r for r in rows if r["name"].lower() == t.param.lower()), None)
    if row is None:
        raise SystemExit(f"--vary '{spec}': {t.stage} has no param '{t.param}'. "
                         f"Names: {', '.join(r['name'] for r in rows)}")
    params = st.setdefault("params", {})
    t.param_key = next((k for k in params if k == row["name"] or k.lower() == row["name"].lower()
                        or k == str(row["id"])), row["name"])
    t.start = float(params.get(t.param_key, row["default"]))
    clo, chi = max(lo, row["min"]), min(hi, row["max"])
    if (clo, chi) != (lo, hi):
        print(f"  note: {key} bounds {lo}:{hi} clipped to the plugin's {clo}:{chi}")
    t.lo, t.hi = clo, chi
    if not clo <= t.start <= chi:
        print(f"  note: {key} starts at {t.start} outside {clo}:{chi}; the first point is clipped")
    return t


def apply_values(cfg, targets, values):
    out = copy.deepcopy(cfg)
    for t, v in zip(targets, values):
        node = _stage_of(out, t)
        (node if t.stage is None else node.setdefault("params", {}))[t.param_key] = float(v)
    return out


# ---------------------------------------------------------------------------
# Loss
# ---------------------------------------------------------------------------

def parse_loss(spec):
    """'ref:ref.wav,masking=2,loudness' → ({term: weight}, ref_path|None)."""
    terms, ref = {}, None
    for raw in filter(None, (s.strip() for s in spec.split(","))):
        if raw.startswith("ref:"):
            ref = raw[4:]
            terms.setdefault("band", 1.0)
            continue
        name, _, w = raw.partition("=")
        if name not in ("band", "loudness", "masking", "level"):
            raise SystemExit(f"--loss: unknown term '{name}' (ref:<file>, loudness, masking, level)")
        terms[name] = float(w) if w else 1.0
    if not terms:
        raise SystemExit("--loss: nothing to measure")
    if ("band" in terms or "loudness" in terms) and ref is None:
        raise SystemExit("--loss: band/loudness need ref:<wav or mixcheck json>")
    return terms, ref


def load_reference(path, span=None):
    """(centred bands, LUFS) from a WAV or a `mixcheck.py --json` profile."""
    p = Path(path)
    if p.suffix.lower() == ".json":
        d = json.loads(p.read_text())
        v = np.array([d["bands_db"][name] for _, _, name in mixcheck.BANDS], dtype=float)
        return v - v.mean(), d.get("lufs_integrated")
    l, r, sr = read_wav(p)
    l, r = cut(l, r, sr, span)
    return balance_bands((l + r) * 0.5, sr), mixcheck.lufs(l, r, sr)[0]


def _absolutise(cfg, base_dir):
    """sdsp-chain resolves `input`/file sidechains against the CWD: pin them."""
    def fix(s):
        return s if s.startswith("track:") or s == "" or os.path.isabs(s) else str(base_dir / s)
    cfg = copy.deepcopy(cfg)
    if isinstance(cfg.get("sidechain"), str):
        cfg["sidechain"] = fix(cfg["sidechain"])
    for tr in cfg.get("track", []):
        if isinstance(tr.get("input"), str):
            tr["input"] = fix(tr["input"])
        if isinstance(tr.get("sidechain"), str):
            tr["sidechain"] = fix(tr["sidechain"])
        for st in tr.get("stage", []):
            if isinstance(st.get("sidechain"), str):
                st["sidechain"] = fix(st["sidechain"])
    for st in cfg.get("master", []):
        if isinstance(st.get("sidechain"), str):
            st["sidechain"] = fix(st["sidechain"])
    return cfg


def _keys_of(tr, cfg):
    """Track names this track keys off (duck_from, track: sidechains)."""
    refs = set()
    if tr.get("duck_from"):
        refs.add(tr["duck_from"])
    for s in [tr.get("sidechain"), cfg.get("sidechain")] + [st.get("sidechain") for st in tr.get("stage", [])]:
        if isinstance(s, str) and s.startswith("track:"):
            refs.add(s[6:])
    return refs


def stem_config(cfg, name):
    """One track alone, pre-master: the tracks it keys off stay as muted keys."""
    by = {t.get("name"): t for t in cfg.get("track", [])}
    keep, todo = set(), [name]
    while todo:
        n = todo.pop()
        if n in keep or n not in by:
            continue
        keep.add(n)
        todo += list(_keys_of(by[n], cfg))
    out = {k: v for k, v in cfg.items() if k not in ("track", "master", "out")}
    out["track"] = []
    for t in cfg.get("track", []):
        if t.get("name") in keep:
            t = copy.deepcopy(t)
            t["mute"] = t.get("name") != name
            out["track"].append(t)
    return out


class Evaluator:
    """Renders a candidate and returns (loss, terms). Deterministic renderer +
    pure measurement = the same x always gives the same loss, so it is cached."""

    def __init__(self, chain, base_cfg, base_dir, targets, terms, ref=None, work=None,
                 span=None, ref_span=None, jobs=None):
        self.chain, self.targets, self.terms = chain, targets, terms
        self.cfg = _absolutise(base_cfg, base_dir)
        self.names = [t.get("name") for t in self.cfg.get("track", [])]
        if len(set(self.names)) != len(self.names) or None in self.names:
            raise SystemExit("every [[track]] needs a unique `name` for stems to be addressable")
        self.work = Path(work or (Path(base_dir) / ".mixopt"))
        self.work.mkdir(parents=True, exist_ok=True)
        self.span = span
        self.ref_bands, self.ref_lufs = (load_reference(ref, ref_span) if ref else (None, None))
        if "loudness" in terms and self.ref_lufs is None:
            raise SystemExit(f"loudness: {ref} carries no LUFS (a mixcheck profile without numpy?)")
        self.jobs = jobs or min(8, os.cpu_count() or 2)
        self.cache = {}
        self.base_levels = None
        self.last_render = None
        self.last_masking = None
        self.n = 0
        self.seconds = []
        if "level" in terms:
            self.base_levels = self._stem_levels(self.cfg)

    # -- rendering ---------------------------------------------------------
    def _render_mix(self, cfg):
        return render(self.chain, cfg, self.work / "eval.toml", self.work / "eval.wav")

    def _render_stems(self, cfg):
        def one(name):
            safe = re.sub(r"[^A-Za-z0-9_.-]+", "_", name)
            return name, render(self.chain, stem_config(cfg, name),
                                self.work / f"stem-{safe}.toml", self.work / f"stem-{safe}.wav")
        with ThreadPoolExecutor(max_workers=min(self.jobs, len(self.names))) as ex:
            return dict(ex.map(one, self.names))

    def _stem_levels(self, cfg):
        return {n: avg_bands(*read_wav(p)) for n, p in self._render_stems(cfg).items()}

    # -- the loss ------------------------------------------------------------
    def measure(self, values):
        key = tuple(round(float(v), 6) for v in values)
        if key in self.cache:
            return self.cache[key]
        t0 = time.time()
        cfg = apply_values(self.cfg, self.targets, values)
        terms = {}
        if "band" in self.terms or "loudness" in self.terms:
            l, r, sr = read_wav(self._render_mix(cfg))
            l, r = cut(l, r, sr, self.span)
            if "band" in self.terms:
                d = balance_bands((l + r) * 0.5, sr) - self.ref_bands
                terms["band"] = float(np.sqrt(np.mean(d * d)))
            if "loudness" in self.terms:
                lufs = mixcheck.lufs(l, r, sr)[0]
                terms["loudness"] = abs(lufs - self.ref_lufs) if math.isfinite(lufs) else 100.0
            self.last_render = self.work / "eval.wav"
        if "masking" in self.terms or "level" in self.terms:
            stems = self._render_stems(cfg)
            audio = {n: read_wav(p) for n, p in stems.items()}
            if "masking" in self.terms:
                bands = {n: bus_bands((a[0] + a[1]) * 0.5, a[2]) for n, a in audio.items()}
                shares = masking_shares(bands)
                terms["masking"] = float(sum(s.sum() for s in shares.values()))
                self.last_masking = masking_report(shares)
            if "level" in self.terms:
                drop = {n: broadband_drop(self.base_levels[n], avg_bands(*a)) for n, a in audio.items()}
                terms["level"] = float(sum(max(0.0, d - LEVEL_FREE_DB) for d in drop.values()))
                terms["_levels"] = {n: round(-d, 2) for n, d in drop.items()}
        loss = sum(self.terms[k] * v for k, v in terms.items() if not k.startswith("_"))
        self.n += 1
        self.seconds.append(time.time() - t0)
        self.cache[key] = (loss, terms)
        return loss, terms


# ---------------------------------------------------------------------------
# Optimiser loop
# ---------------------------------------------------------------------------

class _Budget(Exception):
    """Raised inside the objective once --iters evaluations are spent: scipy's
    maxfev is a soft limit (it finishes the step), this one is exact."""


def optimize(ev, targets, iters, method="nelder-mead", step=0.25, log=None, start=None, quiet=False):
    """Bounded Nelder–Mead/Powell on x ∈ [0,1]^n (each target normalised to its
    bounds). Returns (best_values, best_loss, history) and appends every
    evaluation to `log` (JSONL) as it happens."""
    lo = np.array([t.lo for t in targets])
    hi = np.array([t.hi for t in targets])
    x0 = np.array(start if start is not None else [t.clip(t.start) for t in targets], dtype=float)
    u0 = (x0 - lo) / (hi - lo)
    base = [t.start for t in targets]
    history, best = [], {"loss": math.inf, "values": None, "n": 0}

    def f(u):  # `logf` is bound by the `with` below; the closure reads it at call time
        if iters > 0 and len(history) >= iters:
            raise _Budget()
        v = lo + np.clip(u, 0, 1) * (hi - lo)
        before = ev.n
        loss, terms = ev.measure(v)
        k = len(history) + 1
        rec = {"n": k, "loss": round(loss, 6),
               "terms": {a: (round(b, 4) if isinstance(b, float) else b) for a, b in terms.items()},
               "values": {t.key: round(float(x), 4) for t, x in zip(targets, v)},
               "diff": {t.key: [round(b, 4), round(float(x), 4)] for t, x, b in zip(targets, v, base)
                        if abs(x - b) > 1e-9},
               "s": None if ev.n == before else round(ev.seconds[-1], 3)}
        history.append(rec)
        if logf:
            logf.write(json.dumps(rec) + "\n")
            logf.flush()
        if loss < best["loss"]:
            best.update(loss=loss, values=[float(x) for x in v], n=k)
        if not quiet:
            tt = "  ".join(f"{a}={b:.3f}" for a, b in terms.items() if isinstance(b, float))
            vv = "  ".join(f"{t.key}={x:.3g}" for t, x in zip(targets, v))
            print(f"  {k:>3}  loss {loss:8.4f}  [{tt}]  {vv}{'   *' if k == best['n'] else ''}")
        return loss

    n = len(targets)
    with (open(log, "a") if log else contextlib.nullcontext()) as logf, contextlib.suppress(_Budget):
        if iters <= 0:
            f(u0)
        elif method == "nelder-mead":
            simplex = [u0]
            for i in range(n):
                e = u0.copy()
                e[i] = e[i] + step if e[i] + step <= 1 else e[i] - step
                simplex.append(e)
            opts = {"maxfev": iters, "initial_simplex": np.array(simplex), "xatol": 1e-3,
                    "fatol": 1e-5, "disp": False}
            minimize(f, u0, method="Nelder-Mead", bounds=[(0, 1)] * n, options=opts)
        elif method == "powell":
            minimize(f, u0, method="Powell", bounds=[(0, 1)] * n,
                     options={"maxfev": iters, "xtol": 1e-3, "ftol": 1e-5, "disp": False})
        else:
            raise SystemExit(f"unknown --method {method}")
    return best["values"], best["loss"], history


def read_log(path):
    recs = []
    p = Path(path)
    if p.exists():
        for line in p.read_text().splitlines():
            if line.strip():
                recs.append(json.loads(line))
    return [r for r in recs if "loss" in r]


def _span(s):
    if not s:
        return None
    a, _, b = s.partition(":")
    return (float(a or 0), float(b) if b else None)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("base", help="the mix TOML (sdsp-chain multi-track form)")
    ap.add_argument("--vary", nargs="+", required=True, metavar="TRACK.STAGE.PARAM:LO:HI")
    ap.add_argument("--loss", required=True, help="ref:<wav|json>[,loudness][,masking][,level], term=weight")
    ap.add_argument("--iters", type=int, default=60, help="evaluations (0 = measure the base only)")
    ap.add_argument("--method", default="nelder-mead", choices=["nelder-mead", "powell"])
    ap.add_argument("--step", type=float, default=0.25, help="initial simplex step, fraction of each range")
    ap.add_argument("--out", help="best config, written as TOML (+ <out>.jsonl log, <out>.wav render)")
    ap.add_argument("--resume", action="store_true", help="continue from the best point in <out>.jsonl")
    ap.add_argument("--span", help="measure the mix from START:END seconds")
    ap.add_argument("--ref-span", help="the reference's section, START:END seconds")
    ap.add_argument("--work", help="render directory (default <base dir>/.mixopt)")
    ap.add_argument("--chain", help="sdsp-chain binary (default: $SDSP_CHAIN, $CARGO_TARGET_DIR, …)")
    a = ap.parse_args(argv)

    chain = a.chain or find_chain()
    if not chain:
        raise SystemExit("sdsp-chain not found: cargo build --release -p sdsp-chain, or set SDSP_CHAIN")
    base_path = Path(a.base).resolve()
    cfg = tomllib.loads(base_path.read_text())
    if not cfg.get("track"):
        raise SystemExit(f"{a.base}: needs [[track]] entries (the single-chain form has no stems)")
    terms, ref = parse_loss(a.loss)
    if ref and not os.path.isabs(ref):
        ref = str(base_path.parent / ref) if not Path(ref).exists() else ref
    targets = [parse_vary(s, cfg, chain) for s in a.vary]
    out = Path(a.out).resolve() if a.out else base_path.with_name(base_path.stem + ".best.toml")
    log = out.with_suffix(".jsonl")

    print(f"mixopt  {base_path.name}  chain={chain}")
    print("  loss: " + ", ".join(f"{k}×{w:g}" for k, w in terms.items()) + (f"  ref={ref}" if ref else ""))
    for t in targets:
        print(f"  vary: {t.key:<28} {t.lo:>9.3g} … {t.hi:<9.3g} start {t.start:.4g}")
    ev = Evaluator(chain, cfg, base_path.parent, targets, terms, ref, a.work, _span(a.span),
                   _span(a.ref_span))

    start, done = None, 0
    if a.resume:
        prev = read_log(log)
        if prev:
            b = min(prev, key=lambda r: r["loss"])
            start = [b["values"][t.key] for t in targets]
            done = len(prev)
            print(f"  resume: {done} evaluations logged, best loss {b['loss']:.4f} at #{b['n']}")
    elif log.exists():
        log.unlink()
    if not done:
        log.write_text(json.dumps({"base": str(base_path), "vary": a.vary, "loss_spec": a.loss,
                                   "chain": chain}) + "\n")
    iters = max(0, a.iters - done)
    t0 = time.time()
    best, best_loss, hist = optimize(ev, targets, iters, a.method, a.step, log, start)
    wall = time.time() - t0

    base_loss = hist[0]["loss"] if hist else None
    print(f"\n{len(hist)} evaluations in {wall:.1f} s "
          f"({np.mean(ev.seconds):.2f} s each, {len(ev.names)} tracks)" if ev.seconds else "")
    if base_loss is not None and base_loss > 0:
        print(f"  loss {base_loss:.4f} → {best_loss:.4f} ({100 * (1 - best_loss / base_loss):+.0f}%)")
    traj, cur = [], math.inf
    for r in hist:
        if r["loss"] < cur:
            cur = r["loss"]
            traj.append(f"#{r['n']} {cur:.4g}")
    print("  best-so-far: " + " → ".join(traj))
    for t, v in zip(targets, best or [t.start for t in targets]):
        print(f"  {t.key:<28} {t.start:>9.4g} → {v:<9.4g}")
    if ev.last_masking:
        for row in ev.last_masking[:5]:
            if row["worst_share"] > 0.05:
                print(f"  clash {row['a']} × {row['b']}: {row['hz'][0]}-{row['hz'][1]} Hz "
                      f"{100 * row['worst_share']:.0f}% of the time (Σ bands {row['total']:.2f})")
    if best is not None and iters > 0:
        # Relative inputs survive only next to the base; elsewhere pin them.
        best_cfg = apply_values(cfg if out.parent == base_path.parent else ev.cfg, targets, best)
        out.write_text(dump_toml(best_cfg))
        wav = out.with_suffix(".wav")
        render(chain, apply_values(ev.cfg, targets, best), ev.work / "best.toml", wav)
        print(f"  wrote {out} and {wav.name}; log {log.name}. Listen before keeping it.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
