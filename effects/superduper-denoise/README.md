# SuperDuper Denoise *(planned)*

Neural noise / breath suppressor. STUB — implementation pending.

## Status

Crate scaffolded but not yet a `cdylib` — won't appear in the FX
chain yet. Workspace member so the autotest / build sees it.

## Plan

Port [RNNoise](https://github.com/xiph/rnnoise) (Mozilla, ~85 k
params) inference via the [`nnnoiseless`](https://crates.io/crates/nnnoiseless)
crate. 480-sample frames @ 48 kHz, ~10 ms latency. Bins-band gain
mask multiplied with the STFT magnitude.

## Roadmap

1. Verify `nnnoiseless` is RT-safe enough (no allocations in the hot path)
2. 480-sample ring + 480-sample lookahead for one frame buffering
3. Report 10 ms latency via the CLAP `latency` extension so PDC keeps
   the bus aligned
4. GUI: noise floor read-out + before/after spectrum strip

## Alternatives considered

- **DeepFilterNet** — better quality, ~3-5 MB binary, GPL-licensed
- **RNNoise direct C port** — would need bindgen/cxx — more build complexity
