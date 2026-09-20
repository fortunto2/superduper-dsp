#!/usr/bin/env python3
"""Regenerate the limiter's 4x polyphase inter-sample-peak coefficients.

Usage: uv run --with numpy --with scipy python scripts/gen_os_coefs.py [taps_per_phase]
Paste the printed table over OS_COEFS in effects/superduper-limiter/src/lib.rs.
"""
import sys
import numpy as np
from scipy.signal import firwin

F = 4
TPF = int(sys.argv[1]) if len(sys.argv) > 1 else 12
h = firwin(F * TPF, 1.0 / F, window=("kaiser", 8.6)) * F
print(f"const OS_TAPS_PER_PHASE: usize = {TPF};")
print("const OS_COEFS: [[f32; OS_TAPS_PER_PHASE]; OS_FACTOR] = [")
for p in range(F):
    row = ", ".join(f"{c: .6f}" for c in h[p::F])
    print(f"    [{row}],")
print("];")
print("// phase DC gains:", [round(float(h[p::F].sum()), 5) for p in range(F)])
