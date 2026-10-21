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
passed: a stale binary proves nothing about the code being committed. Build
it first (`./scripts/build_<name>_bundle.sh`). Exit 1 on any failure or on
UNCHECKED when a plugin was explicitly requested.

Needs `clap-validator` on PATH (`cargo install --git
https://github.com/free-audio/clap-validator --tag 0.3.2` on rustc 1.93; the
latest tag wants 1.95).
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
    # The Preset param recalls every other param on the main thread. The
    # validator runs on_main_thread() right after flush() but not after
    # process(), so the two paths read back differently whatever the plugin
    # does. Fix on the plan: the selector becomes a mailbox doc, not a param.
    ("pad", "state-reproducibility-flush"): "Preset param recall — docs/plan/bus-key.md",
    ("wind", "state-reproducibility-flush"): "Preset param recall — docs/plan/bus-key.md",
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
    p = subprocess.run(cmd, capture_output=True, text=True)
    try:
        return json.loads(p.stdout), p.stderr
    except json.JSONDecodeError:
        sys.exit(f"clap-validator produced no JSON (exit {p.returncode}):\n{p.stderr[-800:]}")


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
    if bundles:
        report, err = validate(bundles)
        if a.json:
            Path(a.json).write_text(json.dumps(report, indent=1))
        for pid, tests in report.get("plugin-tests", {}).items():
            name = pid.split(".")[-1]
            bad = [t for t in tests if t["status"]["code"] not in ("success", "skipped")]
            hard = [t for t in bad if (t["status"]["code"] != "warning" or t["name"] in WARN_AS_FAIL) and (name, t["name"]) not in KNOWN]
            fails += bool(hard)
            if not bad:
                print(f"ok         {name:26s} {len(tests)} tests")
            for t in bad:
                if (name, t["name"]) in KNOWN:
                    print(f"{'known':10s} {name:26s} {t['name']}: {KNOWN[(name, t['name'])]}")
                    continue
                tag = "FAIL" if t in hard else "warn"
                print(f"{tag:10s} {name:26s} {t['name']}: {str(t['status'].get('details', ''))[:150]}")
        for path, tests in report.get("plugin-library-tests", {}).items():
            for t in tests:
                if t["status"]["code"] not in ("success", "skipped"):
                    print(f"warn       {Path(path).stem:26s} {t['name']}: {t['status'].get('details', '')}")
        if "CLAP_PLUGIN_ERROR" in err:
            n = err.count("CLAP_PLUGIN_ERROR")
            print(f"note       {n} plugin-side errors on stderr (state/params) — details with --json")
    print(f"\n{len(bundles)} validated, {fails} failing, {len(unchecked)} unchecked")
    sys.exit(1 if fails or (unchecked and (a.only or a.changed)) else 0)


if __name__ == "__main__":
    main()
