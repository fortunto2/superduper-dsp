# SuperDuper NAM

Neural Amp Modeler — neural-network emulation of guitar amps, tube preamps,
and stompboxes. Loads community `.nam` files (WaveNet / LSTM / Linear)
through a pure-Rust inference port of [Steven Atkinson's NeuralAmpModelerCore](https://github.com/sdatkinson/NeuralAmpModelerCore).

| | |
|---|---|
| Category | Effect (audio-in → audio-out) |
| Stereo | yes (independent per-channel network state) |
| Sidechain | no |
| Latency | 0 samples (causal inference) |

## What it does

Runs a small WaveNet (4-50 layers, 8-16 channels) or LSTM (1-3 layers,
8-40 hidden units) per sample on each channel. A trained `.nam` file
captures the non-linear behaviour of a hardware preamp, tube amp, or
overdrive pedal. Drop one on the plugin window or paste its URL — the
plugin loads it on the fly. Built-in tube preamp default model if no
`.nam` is selected.

## Parameters

| Param | Range | Default | What |
|---|---|---|---|
| Input | -24..+24 dB | 0 | Pre-network gain |
| Drive | 0..12 dB | 3 | Pushes the network further into non-linear range |
| Output | -24..+24 dB | 0 | Post-network gain |
| Mix | 0..1 | 1 | Wet/dry blend |
| Tone | -1..+1 | 0 | Post-network tone tilt (low-shelf ±6 dB) |

## Where to find models

| Site | Notes |
|---|---|
| [ToneHunt](https://tonehunt.org) | Biggest library, free account required to download |
| [Tone3000](https://tone3000.com) | NAM + AIDA-X models, direct download links |
| [NAM Hub](https://nam.parametric.audio) | Curator picks |
| [GitHub example_models](https://github.com/sdatkinson/NeuralAmpModelerCore/tree/main/example_models) | Reference models from sdatkinson |

## How to load

1. **Drag-and-drop** — pull a `.nam` file from your browser or Finder
   onto the plugin window. Auto-copies into `~/.superduper-dsp/nam/`
   and auto-loads.
2. **URL paste** — drop a direct `.nam` URL into the `url:` field,
   press Enter or `[download]`. Background `curl` worker, never blocks
   the GUI.
3. **Manual** — drop files into `~/.superduper-dsp/nam/` and hit
   `[reload]`. `[open folder]` opens the directory in your file
   manager.

The library list shows architecture badges (`WaveNet` / `LSTM` /
`Linear`) and tags unsupported models in dim/orange. Prev/next arrows
skip unsupported entries. `[×]` deletes a file from disk with a confirm
prompt.

## Supported architectures

- **WaveNet** — Standard, Lite, Nano. Gating modes None / Gated /
  Blended (Sigmoid or other secondary activation). head1x1, layer1x1.
  Softsign added on top of Tanh / ReLU / Sigmoid / Hardtanh.
- **LSTM** — any number of layers, any hidden size.
- **Linear** — `receptive_field`-tap FIR with optional bias.

## Not supported (explicitly rejected)

- **FiLM** modulation (`conv_pre_film`, `input_mixin_post_film`, etc.) —
  no community models use it.
- **Grouped / depthwise convolutions** (`groups_input != 1`).
- **Post-stack head** (multi-conv1d cascade after the layer arrays).

Rejected files stay in the library, marked `(unsupported)` with a
tooltip explaining why.

## Autotest

```bash
cargo run --release -p nam-test
```

Loads every `.nam` in the library, runs silence / DC / 1 kHz sine /
50 Hz→8 kHz log sweep probes, asserts finite + non-trivial RMS. Exit
code non-zero on failure so CI can gate on it.

## DSP details

- Weight ordering bit-compatible with NAM 0.5.x (see
  `synth_core::nam` for line-by-line references to the C++ source).
- Verified: `wavenet_a1_standard.nam` (13802 params) and
  `lstm_example.nam` (70 params) load + run identity-checks
  successfully.
- Inference is sample-by-sample with per-layer ring-buffer history —
  no batched matmuls, no allocations in `process()`.
- Lock-free model swap via `parking_lot::Mutex::try_lock` on a
  pending box; audio thread never blocks while a new model loads.
