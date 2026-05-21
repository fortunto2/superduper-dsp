# SuperDuper DSP

Open-source CLAP plugin suite — 17 focused effects and synths written in
Rust. Full vocal-chain, three original synths (wavetable bass, jaw-harp
physical model, drum machine), a polyphonic WAV sampler with pitch
tuner, a Mobius-style live looper, custom egui-based GUIs with a retro
phosphor-green theme, factory presets, sidechain ducking, automation
write, MIDI CC / pitch-bend, tempo sync, and CLAP state persistence
across the family.

[**Download the latest release**](https://github.com/fortunto2/superduper-dsp/releases/latest)
· [Install instructions](INSTALL.md) · [Project notes](CLAUDE.md)

## The plugins

### Effects

| Plugin | Use | Highlights |
|---|---|---|
| **EQ** | tone shaping | 3-band parametric (low shelf + mid peak + high shelf) + HP/LP. RBJ biquad math. |
| **Compressor** | dynamics | Soft-knee feed-forward, peak+LP detector, 2 ms lookahead, sidechain HPF, external sidechain port, Clean / Pump / Smooth curves, oversampled ceiling clipper, live GR meter + oscilloscope. |
| **Saturator** | warmth | Tape / Tube / Soft-tanh with Tone tilt + 2×/4× polyphase oversampling. |
| **Delay** | rhythm/space | 3rd-order Lagrange interpolation, tape-style feedback saturation, Stereo / Ping-Pong / Slap modes, sidechain ducking. |
| **Reverb** | space | Dattorro figure-of-eight plate with modulated allpasses, Lagrange-3 fractional taps for click-free SIZE sweeps. Sidechain ducking. |
| **Supermass** | wash | Valhalla-style cascade (reverb → stereo chorus → reverb, 28 s tail), sidechain ducking. |
| **Limiter** | mastering | Lookahead brickwall with 4× true-peak detection on a sidechain upsampler, live GR meter. |
| **Spectrum** | metering | Pass-through analyzer — Spectrum / Spectrogram / Split view, three colour palettes. |
| **Vocal** | restoration | Split-band de-esser + ratio-detector de-clicker tuned for rap/spoken word. |
| **Chorus** | modulation | Multi-tap modulated delay with band-named factory presets (Joy Division Atmosphere → Cocteau Twins shimmer → Vangelis Blade Runner CS-80 lushness). |
| **Looper** | live performance | Mobius-style 4-track live looper, 60 s/track, host-BPM sync with bar-aligned quantize, per-track Feedback for tape-style overdub decay, MIDI CC control for hands-free hardware triggering. |
| **Filter** | sweep / motion | Multi-mode resonant (LP/HP/BP/Notch) + Drive (Tanh/Tape/Tube) + LFO (free + tempo sync) + Env Follow. Designed for Daft-Punk style filter sweeps on the master bus. |
| **MidSide** | stereo width | L/R ↔ M/S encode/decode + per-channel Mid/Side gain + Width. Three modes: in-place Width, Encode →, ← Decode for inserting M/S processors. |
| **LinEq** | mastering EQ | Linear-phase 3-band FIR (~21 ms latency reported to host PDC). Same RBJ biquad target curve, then iFFT-designed symmetric 2048-tap kernel + circular-history convolution. |
| **Soothe** | resonance suppressor | 24-band log-spaced filter bank measures per-band envelopes; baseline = mean of 4 neighbours; bands above baseline + Sensitivity get a dynamic peaking-EQ cut. Tames rolled-r resonances, harsh `s`/`sh`, mud peaks. Soft/Sharp/Hard modes. |
| **NAM** | neural amp modeler | Pure-Rust port of Steven Atkinson's [Neural Amp Modeler](https://github.com/sdatkinson/NeuralAmpModelerCore) inference. Loads community `.nam` files (WaveNet / LSTM / Linear). In-plugin library browser: drag-and-drop import, URL download, prev/next arrows, filter, delete, in-app links to ToneHunt / Tone3000 / NAM Hub. |

### Instruments

| Plugin | Use | Highlights |
|---|---|---|
| **Pad** | polyphonic pad | 8-voice MIDI synth, TPT/ZDF SVF + tanh, click-free voice steal, soft-fade choke, MIDI CC + pitch-bend. |
| **Ambient** | autonomous drone | No-input chord-drone generator. |
| **Wave** | wavetable bass / lead | Mouse-editable curve (sharp / smooth nodes via Catmull-Rom, RDP simplify, Undo/Redo), mip-mapped anti-aliasing, unison + sub + noise + filter env + LFO with 3 destinations + tempo sync, MIDI CC + pitch-bend + aftertouch. |
| **Kubyz** | jaw-harp / khomus | 16-harmonic additive + 3-band bandpass formant + interactive IPA vowel pad + animated mouth trajectory (Circle / Sine / Figure-8 / Triangle / Line) with stereo motion + tempo-sync Mouth Rate + Tongue Pitch + Bashkir / Khomus / Real-D2 presets + tools/kubyz_analyser for fitting your own. |
| **Drum** | drum machine | 6 analog-synthesis voices — Kick / Snare / HH closed / HH open / Clap / Cowbell on consecutive white keys C-D-E-F-G-A. On-screen mini-keyboard hint, mouse-click pads, MIDI passthrough so a single MIDI clip can drive both Drum and bass (Wave/Kubyz) layered. |
| **Sampler** | polyphonic WAV player | Recursive `~/Music/SuperDuper Samples/` scan with subfolder pack picker, configurable root folders persisted to disk. Per-voice multi-mode TPT/ZDF SVF filter (LP/HP/BP/Notch) with `Env→Cutoff` modulation, Reverse playback (one-shot), Velocity→Amp/Cutoff, click-to-audition on the waveform. YIN-style pitch tuner shows the sample's native note + cents + the played note after Tune/Fine, with `→ Root` button to snap Root to the detected pitch. |

All twenty-three share:

- **Sidechain ports** wherever it makes sense (Reverb, Supermass, Delay,
  Saturator, Compressor). Classic use case — route plugin on an aux/send,
  feed dry vocal into the Sidechain port, plugin wet ducks under vocal
  phrases for a clear, modern mix.
- **Custom egui GUI** with shared retro-phosphor theme, monospace
  layout, ASCII section headers, and factory presets.
- **`[bNNNNN]` build-number suffix** in the display name so you always
  know which build is loaded.
- **Three formats** — CLAP (REAPER, Bitwig, Studio One, FL Studio,
  Logic 11+, MultitrackStudio), VST3 (DaVinci Resolve, Cubase,
  Ableton, FL, Studio One), Audio Unit v2 (Logic Pro, GarageBand,
  MainStage, any AU host). VST3 + AU are clap-wrapper bridges; they
  dynamically load the matching `.clap` at runtime.
- **Automation write** — every GUI knob move shows up in the host's FX
  automation lane, including pad drags, harmonic-bar editing and preset
  picks.
- **CLAP state extension** — project save / FX-chain preset round-trips
  all params + bypass; Wave preserves the drawn frame_a curve, Kubyz
  preserves the 16 harmonics + formant bandwidths/gains.
- **A/B compare + Initialize** — standard DAW workflow with the four-
  button bar in every plugin.
- **Live spectrum strip** — log Hz × dB magnitude under the A/B bar.
- **Pitch bend + MIDI CC** (synths only) for live expressive control
  without touching the automation lane.
- **Tempo sync** — Wave LFO Rate and Kubyz Mouth Rate lock to host BPM
  with musical divisions (1/1 ↔ 1/16t, dotted + triplet).

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

Prerequisites:
- Rust stable (`rustup install stable`)
- Apple Silicon Mac, Windows x64, or Linux for CLAP
- CMake ≥ 3.21 (for VST3 / AU wrappers, macOS only)

### Quick path — Makefile

```bash
make all              # CLAP + VST3 + AU, installed to ~/Library/Audio/Plug-Ins/
make clap             # Just the 17 .clap bundles
make wrappers         # VST3 + AU (depends on clap)
make wave             # One plugin (any of the 17 — name matches the crate)
make test             # cargo test --release --workspace
make test-fast        # Skip slow clack-host audits (smoke + lib tests only)
make release VERSION=0.11.0   # Versioned signed zips in ./dist/
make clean            # Wipe cargo + cmake outputs
```

`make` (no args) is the same as `make all`. Plain-shell equivalents
below if you'd rather skip make:

```bash
# All 17 plugins in one go:
./scripts/build_all_bundles.sh
# Or one plugin at a time:
./scripts/build_kubyz_bundle.sh
./scripts/build_reverb_bundle.sh
./scripts/build_sampler_bundle.sh
# ...
```

Per-plugin scripts compile the Rust cdylib, package the `.clap`
bundle (macOS: directory with `Contents/MacOS/` + `Info.plist`,
Windows: single `.clap` file), ad-hoc sign on macOS, and install into
`~/Library/Audio/Plug-Ins/CLAP/`.

The combined `scripts/build_release.sh <version>` produces signed
zips ready to ship.

### CLAP + VST3 + AU — full path (macOS arm64)

```bash
# 1. CLAP bundles first (the wrappers need them on disk at runtime):
./scripts/build_all_bundles.sh

# 2. Pull the clap-wrapper submodule + build VST3 + AU wrappers:
git submodule update --init --recursive
./scripts/build_wrappers.sh --install
```

The wrapper script applies local clap-wrapper patches needed for the
macOS 26 SDK, runs CMake against the submodule, builds 17 × `.vst3` +
17 × `.component`, and with `--install` drops them into
`~/Library/Audio/Plug-Ins/VST3/` and `~/Library/Audio/Plug-Ins/Components/`.

VST3 and AU wrappers are pure CLAP loaders — they dynamically `dlopen`
the matching `.clap` from the system CLAP folder at runtime. Don't
delete the `.clap` after building the wrapper.

### Running tests

```bash
make test                                                          # everything
make test-fast                                                     # skip slow audits
cargo test --release -p superduper-wave --test mod_matrix_audit -- --nocapture   # e2e WAV audit
cargo test --release -p superduper-reverb --test spectrum -- --nocapture         # ASCII spectrum
cargo test --release -p superduper-compressor --test quality_audit -- --nocapture # THD + aliasing
```

Several plugins ship realistic end-to-end tests that boot the plugin
through `clack-host`, send MIDI / CC / param events, and write a WAV
to `/tmp/` for human listening — `afplay /tmp/wave_modmatrix_active.wav`
on macOS, for example.

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
sdk/                  — CLAP plumbing helpers (ParamDef, apply_param_events,
                        emit_dirty_param_events, emit_gesture_events,
                        save/load_simple_state, output_slice, …)
sdk-build/            — build.rs helper that injects SDSP_BUILD_NUM / DATE
sdk-macros/           — proc-macro params!{} (M2)
synth-core/           — shared DSP blocks (Biquad, DelayLine, Ducker,
                        EnvelopeDetector, SmoothedParam, PadVoice, AdsrEnvelope,
                        Oversampler2x, SlewLimiter2Pole, …) + analysis
                        (FFT, ASCII spectrogram, sine sweep, THD/IMD/aliasing
                        measurement) + GUI helpers (theme, section, param_row,
                        learn_param_row_g, MidiLearnState, LiveScope,
                        AbSnapshot, top_bar, ab_init_bar, presets)
effects/superduper-*/ — 17 plugins (11 effects + 6 instruments)
cmake/                — plugin_list.cmake (VST3/AU build manifest)
CMakeLists.txt        — drives the clap-wrapper VST3/AU build
Makefile              — single entry point: `make all` / `make wave` / `make test`
scripts/
  build_*_bundle.sh   — per-plugin .clap packagers
  build_all_bundles.sh — all 17 in one shot
  build_release.sh    — versioned signed release zips
  build_wrappers.sh   — VST3 + AU wrappers via clap-wrapper (macOS)
tools/sdsp-runner/    — standalone CLAP host (effects only, file → cpal)
tools/clap-wrapper/   — git submodule (free-audio/clap-wrapper)
tools/clap-wrapper-patches/ — local patches for macOS 26 SDK compat
tools/kubyz_analyser/ — Python FFT analyser for fitting new Kubyz presets
.github/workflows/    — release CI (macos-14 builds CLAP + VST3 + AU,
                        windows-latest builds CLAP only)
```

## License

MIT. See [LICENSE](LICENSE).
