# SuperDuper DSP

Open-source CLAP plugin suite — a full vocal-chain in eight focused effects
written in Rust. Custom egui-based GUIs with a retro phosphor-green theme,
factory presets, and sidechain ducking across the family.

[**Download the latest release**](https://github.com/fortunto2/superduper-dsp/releases/latest)
· [Install instructions](INSTALL.md) · [Project notes](CLAUDE.md)

## The plugins

| Plugin | Use | Highlights |
|---|---|---|
| **EQ** | tone shaping | 3-band parametric (low shelf + mid peak + high shelf) + HP/LP. RBJ biquad math. |
| **Compressor** | dynamics | Soft-knee feed-forward, peak+LP detector, 2 ms lookahead, sidechain HPF, **external sidechain** port, live GR meter. |
| **Saturator** | warmth | Tape / Tube / Soft-tanh curves with Tone tilt and DC blocker. |
| **Delay** | rhythm/space | 3rd-order Lagrange interpolation, tape-style feedback saturation, Stereo / Ping-Pong / Slap modes, sidechain ducking. |
| **Reverb** | space | Dattorro figure-of-eight plate with modulated allpasses, cross-feedback, sidechain ducking. |
| **Supermass** | wash | Valhalla-style cascade (reverb → stereo chorus → reverb, 28 s tail), sidechain ducking. |
| **Limiter** | mastering | Lookahead brickwall with 4× true-peak detection on a sidechain upsampler, live GR meter. |
| **Spectrum** | metering | Pass-through analyzer — Spectrum / Spectrogram / Split view, three colour palettes. |

All eight share:

- **Sidechain ports** wherever it makes sense (Reverb, Supermass, Delay,
  Saturator, Compressor). Classic use case — route plugin on an aux/send,
  feed dry vocal into the Sidechain port, plugin wet ducks under vocal
  phrases for a clear, modern mix.
- **Custom egui GUI** with shared retro-phosphor theme, monospace
  layout, ASCII section headers, and factory presets.
- **`[bNNNNN]` build-number suffix** in the display name so you always
  know which build is loaded.
- **CLAP** format — runs in REAPER, Bitwig, Studio One, FL Studio,
  MultitrackStudio, etc.

## Quick start

### Install

Grab the latest zip from
[Releases](https://github.com/fortunto2/superduper-dsp/releases/latest),
unzip, drop the `.clap` bundles into your CLAP folder, and rescan your
host. Full step-by-step + macOS Gatekeeper notes in
[INSTALL.md](INSTALL.md).

### Vocal-send-ducked pattern

The "Vocal Send Ducked" preset (available on Reverb, Supermass, and
Delay) is the classic pop-vocal trick:

1. On the vocal track: dry vocal only (maybe EQ → Compressor first).
2. New aux/send track: insert SuperDuper Reverb (or Delay/Supermass).
3. In REAPER: right-click plugin → **Pin Connector** → route the
   vocal track's signal into the plugin's **Sidechain** port (pins 3-4).
4. Pick the **"Vocal Send Ducked"** preset.
5. The reverb wet now ducks down whenever the vocal speaks — vocal
   stays clear, reverb fills the silences.

## Building from source

Apple Silicon (or Windows / Linux native):

```bash
cargo build --release \
    -p superduper-reverb \
    -p superduper-supermass \
    -p superduper-spectrum \
    -p superduper-saturator \
    -p superduper-delay \
    -p superduper-compressor \
    -p superduper-eq \
    -p superduper-limiter
```

Per-plugin install scripts under `scripts/` build the .clap bundle and
drop it into `~/Library/Audio/Plug-Ins/CLAP/` on macOS. The combined
`scripts/build_release.sh <version>` produces signed zips ready to ship.

## Standalone runner

`tools/sdsp-runner` is a tiny CLAP host that loads any `.clap` bundle
and plays a WAV file through it to your speakers via cpal. Useful
during DSP development — much faster iteration than restart-REAPER.

```bash
cargo run --release -p sdsp-runner -- \
    ~/Library/Audio/Plug-Ins/CLAP/SuperDuperReverb.clap test-vocal.wav
```

## DSP highlights (research-driven)

- **Reverb** — Dattorro 1997 "Effect Design Part 1" figure-of-eight
  topology. Modulated allpasses + cross-feedback for true stereo.
- **Compressor** — Giannoulis-Massberg-Reiss "Digital Dynamic Range
  Compressor Design" (JAES 2012). Quadratic soft-knee, peak+LP detector.
- **EQ** — Robert Bristow-Johnson "Cookbook formulae for audio EQ
  biquad filter coefficients" (W3C TR). Symmetric peaking
  (boost N dB + cut N dB at same f/Q = unity).
- **Delay** — 3rd-order Lagrange fractional delay (J.O. Smith,
  Physical Audio Signal Processing). 2-pole slew for tape doppler.
- **Limiter** — FabFilter Pro-L style architecture: lookahead delay
  through Lagrange interpolation, 4× FIR upsampler for true-peak ISP
  detection, asymmetric attack/release envelope.

## Workspace layout

```
sdk/                — CLAP plumbing helpers (ParamDef, apply_param_events, …)
sdk-build/          — build.rs helper that injects SDSP_BUILD_NUM / DATE
sdk-macros/         — proc-macro params!{} (M2)
synth-core/         — shared DSP blocks (Biquad, DelayLine, Ducker,
                      EnvelopeDetector, …) + analysis (FFT, ASCII
                      spectrogram, sine sweep frequency response) +
                      GUI helpers (style, section, param_row, presets)
effects/superduper-*/   — eight plugins
tools/sdsp-runner/  — standalone CLAP host
.github/workflows/  — release CI (macos-14 + windows-latest)
```

## License

MIT. See [LICENSE](LICENSE).
