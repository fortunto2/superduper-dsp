"""Analyse a kubyz reference recording: extract f0, 16 harmonic amplitudes,
and the 3 strongest spectral formants. Print results in a form ready to
paste into Rust as a Kubyz preset."""

import sys
import numpy as np
import scipy.io.wavfile as wav
from scipy.signal import find_peaks

PATH = sys.argv[1] if len(sys.argv) > 1 else "/Users/rustam/Music/1music/my songs/Media/kubiz1000.wav"
N_HARMONICS = 16

sr, raw = wav.read(PATH)
if raw.ndim > 1:
    raw = raw.mean(axis=1)
if raw.dtype.kind == "i":
    raw = raw.astype(np.float32) / float(np.iinfo(raw.dtype).max)
else:
    raw = raw.astype(np.float32)

dur = len(raw) / sr
peak = float(np.abs(raw).max())
rms = float(np.sqrt((raw**2).mean()))
print(f"file:        {PATH}")
print(f"sr={sr}  duration={dur:.3f}s  peak={peak:.3f}  rms={rms:.4f}")

# --- fundamental via autocorrelation on a sustain window -----------------
sus_start = int(0.05 * sr)
sus_end = min(len(raw), int(0.95 * sr))
sus = raw[sus_start:sus_end]
ac = np.correlate(sus, sus, mode="full")[len(sus) - 1 :]
ac = ac / (ac[0] + 1e-12)
lo, hi = int(sr / 400), int(sr / 40)
peak_idx = lo + int(np.argmax(ac[lo:hi]))
f0 = sr / peak_idx
midi = 69 + 12 * np.log2(f0 / 440)
print(f"f0 ≈ {f0:.1f} Hz  (period {peak_idx} samples, MIDI ≈ {midi:.1f})")

# --- spectrum via FFT, Hann windowed -------------------------------------
N = 1 << int(np.ceil(np.log2(min(len(sus), 32768))))
win = np.hanning(N)
spec = np.fft.rfft(sus[:N] * win)
mag = np.abs(spec) / (N / 2)
freqs = np.fft.rfftfreq(N, d=1 / sr)

# --- harmonic amplitudes -------------------------------------------------
def amp_at(f_hz, bin_tol=3):
    bin0 = int(round(f_hz * N / sr))
    lo = max(0, bin0 - bin_tol)
    hi = min(len(mag), bin0 + bin_tol + 1)
    return float(mag[lo:hi].max())

h_amps = np.array([amp_at(f0 * (n + 1)) for n in range(N_HARMONICS)])
# normalise relative to h1
h_lin = h_amps / max(h_amps[0], 1e-9)
h_db = 20 * np.log10(np.maximum(h_lin, 1e-6))
print()
print("Harmonics (relative to H1):")
print("  n | f (Hz) | linear |  dB below H1")
for n, (lin, db) in enumerate(zip(h_lin, h_db)):
    print(f"  {n+1:2d} | {f0*(n+1):6.1f} | {lin:6.3f} | {-db:7.2f}")

# --- formant detection via spectral envelope peaks ----------------------
# Smooth the magnitude in dB to find broad maxima (formant centres).
smag = mag.copy()
# Restrict to a useful range and apply log scale to find envelope peaks
log_mag = 20 * np.log10(np.maximum(smag, 1e-9))
# Smooth with a 50-bin moving average to suppress harmonic spikes.
kernel = np.ones(50) / 50
log_smooth = np.convolve(log_mag, kernel, mode="same")
# Search for peaks in 200-3500 Hz range.
bin_lo = int(200 * N / sr)
bin_hi = int(3500 * N / sr)
peaks, props = find_peaks(log_smooth[bin_lo:bin_hi], distance=int(150 * N / sr))
peak_bins = peaks + bin_lo
# Sort peaks by height (envelope amplitude), take strongest 3.
peak_heights = log_smooth[peak_bins]
order = np.argsort(-peak_heights)[:6]
formants = sorted([(freqs[peak_bins[i]], peak_heights[i]) for i in order])[:3]
print()
print("Formant peaks (smoothed envelope):")
for i, (f, h) in enumerate(formants):
    print(f"  F{i+1} ≈ {f:6.1f} Hz   (envelope level {h:+5.1f} dB)")

# Estimate bandwidth — width where the smoothed envelope drops 3 dB.
def estimate_bw(centre_hz):
    centre_bin = int(centre_hz * N / sr)
    peak_db = log_smooth[centre_bin]
    threshold = peak_db - 3.0
    # walk left
    left = centre_bin
    while left > 0 and log_smooth[left] > threshold:
        left -= 1
    right = centre_bin
    while right < len(log_smooth) - 1 and log_smooth[right] > threshold:
        right += 1
    return (right - left) * sr / N

print()
print("Suggested Rust preset (paste into presets.rs):")
print()
print("// from kubiz1000.wav")
print(f"// f0 ≈ {f0:.1f} Hz  (MIDI {midi:.1f})")
print("let db: [f32; N_HARMONICS] = [")
# emit relative-dB (positive = N dB below H1) so it matches db_to_lin_array
for chunk in [h_db[i:i+8] for i in range(0, N_HARMONICS, 8)]:
    print("    " + ", ".join(f"{-d:6.2f}" for d in chunk) + ",")
print("];")
print(f"// Formant: F1={formants[0][0]:.0f} Hz, F2={formants[1][0]:.0f} Hz, F3={formants[2][0]:.0f} Hz")
bws = [estimate_bw(f) for f, _ in formants]
print(f"// BW:     bw1={bws[0]:.0f},  bw2={bws[1]:.0f},  bw3={bws[2]:.0f}")
