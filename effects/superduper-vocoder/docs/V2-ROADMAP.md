# SuperDuper Vocoder — v2 roadmap (spectral upgrade + useful viz)

Goal: lift the vocoder from a solid 16-band channel vocoder to top-tier
(iZotope / MVocoder / EMS class) with **selectable modes**, a genuinely useful
**visualization**, and **zero code duplication** (shared spectral core with
SuperDuper Pitch).

Do it in the phases below, in order. Each phase ships with tests + a green
`cargo test --release --workspace`. Don't start a phase until the previous one
is green.

---

## Phase 0 — shared spectral core (NO duplication) — FOUNDATION

SuperDuper Pitch already has a working STFT phase vocoder in
`effects/superduper-pitch/src/pvoc.rs` (realfft, N=2048, hop=512, 75 % overlap,
window + OLA + FFT-plan management, RT-safe). The spectral vocoder below needs
the **same STFT scaffolding** but a different per-frame operation
(cross-synthesis instead of pitch shift). So:

- Extract the reusable STFT scaffolding into **`synth_core::spectral`** (new
  module): an `StftProcessor` (or `Stft` analyzer + resynth) that owns
  windowing, the analysis/synthesis ring buffers, OLA normalization (the
  `win.max(1.0)` COLA lesson from the pitch fix), and the realfft plans —
  pre-allocated, RT-safe, alloc-free `process()`. It takes a **per-frame
  callback** that receives the analysis spectrum(s) and fills the synthesis
  spectrum.
- Refactor `superduper-pitch/src/pvoc.rs` to build its pitch-shift op **on top
  of** the shared `StftProcessor` (keep all 20 pitch tests green — behaviour
  identical).
- The scaffolding must support a **dual-input** frame (modulator FFT + carrier
  FFT) so the vocoder can combine them; pitch only uses one input.

Test: pitch crate stays 20/20 green after the refactor.

---

## Phase 1 — Spectral vocoder mode (the big win) + Mode param

- New param **`Mode` {Classic, Spectral}** (enum, `dirty_choice_row_g`, dirty
  #24, persisted). Classic = the existing 11/16/20-band channel vocoder (do NOT
  regress it). Spectral = FFT cross-synthesis via the shared `StftProcessor`.
- **Spectral cross-synthesis:** per frame, take the **modulator magnitude
  spectrum**, smooth it into a spectral envelope (a `Bands` / smoothing control
  gives the classic-few ↔ ultra-detailed continuum, e.g. 32…512 effective
  bins), and **apply it to the carrier's spectrum** (carrier phases, modulator
  magnitude envelope), then iFFT + OLA. Carrier = internal osc or sidechain,
  same as Classic.
- `Formant Shift` in Spectral = shift the modulator envelope up/down before
  applying (frequency-warp the envelope). `Unvoiced` and `Drive` still apply.
- Latency: Spectral adds the STFT window latency → report via the CLAP latency
  ext; Classic stays 0-latency. Switching Mode must not glitch (handle the
  latency change cleanly / report the max).
- Test: Spectral mode on a voice + saw carrier → output carries the carrier's
  harmonic structure shaped by the modulator's formant envelope (the formant
  peaks of the output track the modulator, like the Classic band test but
  finer). Classic mode output unchanged (regression guard).

---

## Phase 2 — Useful visualization (replace the generic EQ strip)

The current spectrum strip is a plain output-spectrum EQ view — not vocoder-
specific. Replace/augment with a **vocoder activity display** that shows the
vocoding actually happening:

- **Classic mode:** a row of **live per-band envelope bars** — one bar per
  active band (11/16/20), height = that band's current envelope level (the
  iconic hardware-vocoder bouncing bars). This shows which bands/formants the
  modulator is opening in real time. Colour by level.
- **Spectral mode:** the discrete bars become a **smooth live spectral-envelope
  curve** (the modulator magnitude envelope that's shaping the carrier), filled.
- Optional overlay: faint **output spectrum** behind the bars/curve so you see
  the carrier being shaped. Keep the modulator envelope the hero.
- Backed by a lock-free snapshot (per-band envelope array / envelope curve) the
  audio thread writes and the GUI samples ~30–60 Hz (like `LiveScope`). No
  locks, no alloc in audio thread. Pre-allocate the snapshot arrays.
- Keep it readable in light + dark; label it ("band activity" / "formant
  envelope"), not a generic spectrum.

---

## Phase 3 — True voiced/unvoiced detection (cleaner consonants)

Replace the fixed top-band noise blend with a real per-frame classifier
(spectral flatness / zero-crossing rate / YIN pitch confidence). Voiced frames →
tonal carrier; unvoiced frames → noise carrier. `Unvoiced` becomes the
mix/aggressiveness. MBE-style. Test: sung vowel = voiced (tonal), `sss`/`t` =
unvoiced (noise) — measured HF-noise ratio in unvoiced windows.

---

## Phase 4 — Per-band pan + band matrix (Classic mode, creative)

- **Per-band pan / spread:** spread the bands across the stereo field (a
  `Spread` control) — wide vocoder.
- **Band matrix** (advanced, optional): remap modulator band N → carrier band M
  (formant warp / creative mis-routing), MVocoder-style. Could be a later
  sub-phase; a simple `Band Shift` (offset the mapping) is a cheap first version.

---

## Phase 5 — Ensemble + extras (character)

- **Ensemble:** the VP-330 lush-choir effect on the carrier (BBD-style multi-
  voice chorus). Reuse `superduper-chorus` DSP if cleanly extractable, else a
  small dedicated ensemble.
- **Freeze / Hold:** sustain the current vocoded frame (drone).
- **Frequency shifter** (EMS 5000 character, inharmonic metal) — niche, last.

---

## Constraints (every phase)
- RT-safe: no heap / locks / syscalls in `process()`; pre-allocate in
  `activate()`. Denormal guard stays.
- Don't regress Classic mode or the pitch crate. Full workspace green.
- Param table grows — batch additions where possible; each new param needs
  value_to_text, dirty/gesture, state persistence, and apply_preset coverage.
- Keep the dual-engine philosophy clean: Classic = channel bank, Spectral =
  shared STFT core. One plugin, selectable Mode.
