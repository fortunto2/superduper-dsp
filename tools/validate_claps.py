#!/usr/bin/env python3
"""Run the official CLAP validator over our bundles and say what failed.

    python3 tools/validate_claps.py                   # every built bundle in dist/
    python3 tools/validate_claps.py --installed       # ~/Library/Audio/Plug-Ins/CLAP
    python3 tools/validate_claps.py --changed         # only crates touched in the staged diff
    python3 tools/validate_claps.py --only compressor,eq

clap-validator (free-audio) instantiates each plugin like a strict host and
checks parameters (conversions, fuzzing, wrong namespaces), state round trips
(basic, buffered streams, null cookies, flush) and scan time. It found, on
2026-10-21: text_to_value supported for some parameters and not others in 21
plugins, a stepped parameter with non-integer bounds in Spectrum, and state
that differs between two instances given the same parameters in 4 plugins.

A bundle that is older than its crate's sources is reported UNCHECKED, not
passed: a stale binary proves nothing about the code being committed. A
warning the validator cannot measure fairly for one plugin is listed in
KNOWN with its reason and printed as `known`, never as a pass. The
validator's own source: `git clone https://github.com/free-audio/clap-validator`
(cargo install keeps no checkout). Build
it first (`./scripts/build_plugin_bundle.sh <name>`). Exit 1 on any failure or on
UNCHECKED when a plugin was explicitly requested.

Needs `clap-validator` on PATH: `scripts/install_clap_validator.sh` (or
`make validator-install`) builds 0.4.1 with our patches from
tools/clap-validator-patches/ — a plain `cargo install --git …` works too but
runs the unpatched process-audio-denormals (random events differ between
its two passes). rustc >= 1.95; `--tag 0.3.2` builds on 1.93. Both JSON shapes are read: 0.3 keyed tests by
plugin id, 0.4 lists `results`. A validated bundle with no test entries is
UNKNOWN, never a pass — 0.4.1 printed "0 failing" through the 0.3 reader
until a known-bad plugin was run by hand (2026-10-21).
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PLUGIN_LIST = ROOT / "cmake" / "plugin_list.cmake"
WARN_AS_FAIL = set()  # scan-time warnings are reported, never fatal

# Failures we know the cause of and have decided not to fix yet. Each one
# is printed as `known`, never as a pass, and names its plan entry: deleting
# a line here is how the decision gets revisited.
KNOWN = {
    # Nothing at the moment. The Looper's 8–65× "denormal" reading was the
    # validator's own measurement (tools/clap-validator-patches/ fixes it:
    # same events in both passes, a fresh instance for the second, only the
    # plugin's process() timed); the plugin measures 1.0× on subnormals
    # (effects/superduper-looper/tests/denormals.rs).
}


def plugins():
    """crate -> bundle name, from the one list every build step reads."""
    out = {}
    for line in PLUGIN_LIST.read_text().splitlines():
        m = re.match(r'\s*"(superduper-[a-z0-9-]+)\|(SuperDuper\w+)\|', line)
        if m:
            out[m.group(1)] = m.group(2)
    return out


def newest_source_mtime(crate):
    d = ROOT / "effects" / crate
    files = list((d / "src").rglob("*.rs")) + [d / "Cargo.toml"]
    # shared code the plugin links: a change there is a change to the binary
    for shared in ("sdk/src", "synth-core/src"):
        files += list((ROOT / shared).rglob("*.rs"))
    return max((f.stat().st_mtime for f in files if f.exists()), default=0.0)


def staged_crates():
    diff = subprocess.run(["git", "diff", "--cached", "--name-only"], cwd=ROOT, capture_output=True, text=True).stdout
    names = set()
    shared = False
    for line in diff.splitlines():
        m = re.match(r"effects/(superduper-[a-z0-9-]+)/", line)
        if m:
            names.add(m.group(1))
        if line.startswith(("sdk/", "synth-core/")):
            shared = True
    return names, shared


def validate(bundles):
    cmd = ["clap-validator", "validate", "--json", "--hide-output"] + [str(b) for b in bundles]
    # Factory state, not what the user last drew: Wave, Kubyz and NAM boot on
    # the last user preset from ~/.superduper-dsp unless told otherwise, which
    # read as "default Preset is 0 but the instance says 4" (2026-10-21). A
    # plugin under test starts from its tables, like its own quality tests.
    env = dict(os.environ, SUPERDUPER_WAVE_FACTORY="1", SUPERDUPER_KUBYZ_FACTORY="1", SUPERDUPER_NAM_FACTORY="1")
    p = subprocess.run(cmd, capture_output=True, text=True, env=env)
    try:
        return json.loads(p.stdout), p.stderr
    except json.JSONDecodeError:
        sys.exit(f"clap-validator produced no JSON (exit {p.returncode}):\n{p.stderr[-800:]}")


def results(report):
    """Every test the validator ran, as (bundle stem, scope, test name, status),
    from either JSON shape. scope is "plugin" or "library"."""
    out = []
    if "results" in report:  # 0.4.x
        for e in report["results"]:
            ((kind, t),) = e["test"].items()
            scope = "plugin" if kind == "plugin-instance" else "library"
            out.append((Path(t["path"]).stem, scope, t["test"], e["status"]))
        return out
    for pid, tests in report.get("plugin-tests", {}).items():  # 0.3.x
        out += [(pid, "plugin", t["name"], t["status"]) for t in tests]
    for path, tests in report.get("plugin-library-tests", {}).items():
        out += [(Path(path).stem, "library", t["name"], t["status"]) for t in tests]
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--installed", action="store_true", help="validate the installed bundles instead of dist/")
    ap.add_argument("--changed", action="store_true", help="only crates in the staged diff (pre-commit)")
    ap.add_argument("--only", help="comma-separated crate suffixes, e.g. compressor,eq")
    ap.add_argument("--json", help="write the raw validator report here")
    a = ap.parse_args()
    if not shutil.which("clap-validator"):
        print("UNCHECKED: clap-validator not on PATH (see the docstring)")
        sys.exit(2)
    table = plugins()
    wanted = set(table)
    if a.only:
        wanted = {f"superduper-{s.strip()}" for s in a.only.split(",")}
    if a.changed:
        names, shared = staged_crates()
        wanted = set(table) if shared else names & set(table)
        if not wanted:
            print("validate: no plugin crate in the staged diff — nothing to check")
            return
    base = Path.home() / "Library/Audio/Plug-Ins/CLAP" if a.installed else ROOT / "dist"
    bundles, unchecked = [], []
    for crate in sorted(wanted):
        b = base / f"{table[crate]}.clap"
        bin_ = b / "Contents/MacOS" / table[crate]
        if not bin_.exists():
            unchecked.append((crate, "not built"))
        elif bin_.stat().st_mtime < newest_source_mtime(crate):
            unchecked.append((crate, "bundle older than its sources — rebuild"))
        else:
            bundles.append(b)
    for crate, why in unchecked:
        print(f"UNCHECKED  {crate:26s} {why}")
    fails = 0
    unknown = []
    if bundles:
        report, err = validate(bundles)
        if a.json:
            Path(a.json).write_text(json.dumps(report, indent=1))
        by_bundle = {}
        for stem, scope, test, status in results(report):
            by_bundle.setdefault(stem, []).append((scope, test, status))
        for b in bundles:
            stem = b.stem
            name = stem.removeprefix("SuperDuper").lower()
            tests = by_bundle.get(stem) or by_bundle.get(f"co.superduperai.{name}") or []
            if not tests:
                unknown.append(stem)
                print(f"UNKNOWN    {name:26s} the validator reported no test for this bundle")
                continue
            bad = [t for t in tests if t[2]["code"] not in ("success", "skipped")]
            hard = [t for t in bad if t[0] == "plugin" and (t[2]["code"] != "warning" or t[1] in WARN_AS_FAIL) and (name, t[1]) not in KNOWN]
            fails += bool(hard)
            if not bad:
                print(f"ok         {name:26s} {len(tests)} tests")
            for t in bad:
                if (name, t[1]) in KNOWN:
                    print(f"{'known':10s} {name:26s} {t[1]}: {KNOWN[(name, t[1])]}")
                    continue
                tag = "FAIL" if t in hard else "warn"
                print(f"{tag:10s} {name:26s} {t[1]}: {str(t[2].get('details', '') or '')[:150]}")
        if "CLAP_PLUGIN_ERROR" in err:
            n = err.count("CLAP_PLUGIN_ERROR")
            print(f"note       {n} plugin-side errors on stderr (state/params) — details with --json")
    print(f"\n{len(bundles) - len(unknown)} validated, {fails} failing, {len(unchecked)} unchecked, {len(unknown)} unknown")
    if unknown:
        sys.exit(2)
    sys.exit(1 if fails or (unchecked and (a.only or a.changed)) else 0)


if __name__ == "__main__":
    main()
