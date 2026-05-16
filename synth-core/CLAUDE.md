# synth-core — shared DSP library

Anything reusable across SuperDuper effect/synth plugins lives here.

## Modules

- **`dsp_blocks`** — `Ducker`, `Tilt`, `DcBlocker`, `SmoothedParam`. RT-safe
  building blocks. Default-constructible, process methods take params as
  arguments (no internal state besides what each block actually needs).
- **`analysis`** — FFT (`magnitude_spectrum_db`), ASCII spectrogram
  (`ascii_spectrum`), sine-sweep frequency response. For tests only — not
  RT-safe (uses heap, mutex-guarded planner cache).
- **`supermass`** — Valhalla-style cascade reverb as a fundsp `Net`. Ported
  from rust-synth's `preset.rs`. Caller owns the Net: `set_sample_rate`,
  `tick(in, out)` per sample.

## Adding a new shared block

1. New module file under `src/`, or extend `dsp_blocks.rs` if it's a small
   primitive.
2. `#[derive(Default)]` if possible. Public state is fine if it's documented
   as RT-safe.
3. The `process` method must:
   - Take `&mut self` plus runtime params (sr, gains, times).
   - Never allocate, never use `Mutex` / `RwLock`, never panic.
   - Return one sample (or a stereo pair) — block-rate processing is the
     caller's job.
4. Add tests to `tests/dsp_blocks.rs`. Aim for one positive and one negative
   case (does the thing / doesn't do anything stupid at edges).

## Adding a new fundsp graph

Like `supermass::build_wet()`:
- One function that returns `Net` — no parameters that change Net geometry.
- All runtime-tweakable knobs are applied by the caller as post-process
  (the reverb plugins do mix/width/drive/tilt outside the Net).
- Document the topology in the file header with an ASCII diagram.

## Adding analysis helpers

`analysis.rs` is for *test instrumentation*, not RT code. Use anything you
like (rustfft, image, plotters, etc.) but keep API simple:
- Input: `&[f32]` (mono) or `&[(f32, f32)]` (stereo).
- Output: `String` (for ASCII), `Vec<(f32, f32)>` (for raw data), or
  `Result<(), io::Error>` (for file dumps to /tmp).

## Tests

`tests/dsp_blocks.rs` — 9 unit tests, run with
`cargo test --release -p superduper-synth-core`. New blocks: extend this
file rather than adding new test files; the suite is intentionally small
and fast.
