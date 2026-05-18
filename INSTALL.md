# Installing SuperDuper DSP plugins

Thirteen plugins ship from this repo — a full mix/master chain plus
four instruments.

**Effects (audio in → audio out):**

- **SuperDuper EQ** — 3-band parametric (low shelf, mid peak, high shelf) + HP/LP
- **SuperDuper Compressor** — soft-knee feed-forward with lookahead, sidechain HPF and external sidechain port, live GR meter, PDC
- **SuperDuper Saturator** — tape / tube / soft-clip analog warmth + Tilt EQ, 2×/4× polyphase oversampling
- **SuperDuper Delay** — Lagrange-interp stereo delay with tape-style feedback saturation, ping-pong + slap modes, sidechain ducking
- **SuperDuper Reverb** — Dattorro plate reverb with sidechain ducking
- **SuperDuper Supermass** — Valhalla-style cascade reverb (chorus + double tail), sidechain ducking
- **SuperDuper Limiter** — lookahead brickwall with 4× true-peak detection, live GR meter, PDC
- **SuperDuper Vocal** — split-band de-esser + mouth de-clicker tuned for rap vocals
- **SuperDuper Spectrum** — pass-through analyzer (Spectrum / Spectrogram / Split view, 3 colour palettes)

**Instruments (MIDI in or autonomous → audio out):**

- **SuperDuper Ambient** — autonomous 4-voice chord-drone generator
- **SuperDuper Pad** — polyphonic MIDI synth, 8-voice PadVoice + per-voice ADSR + TPT/ZDF SVF, click-free voice steal
- **SuperDuper Wave** — wavetable bass/lead with mouse-editable curve editor, mip-mapped anti-aliasing, per-voice unison + sub + noise + filter envelope + LFO + 2-slot mod matrix + Undo/Redo
- **SuperDuper Kubyz** — physical-model jaw-harp / khomus, 16-harmonic additive engine + 3-band bandpass formant + interactive IPA vowel pad + animated mouth trajectory + tempo-sync mouth rate

## Plugin formats

- **CLAP** — native for REAPER 7+, Bitwig 4.3+, Studio One 6.5+, FL Studio 21+, MultitrackStudio. Ships for macOS arm64 + Windows x64.
- **VST3** (macOS only for now) — for DaVinci Resolve, Cubase, Ableton Live, FL Studio, Studio One, Logic Pro.
- **Audio Unit v2** (macOS only) — for Logic Pro, GarageBand, MainStage and any AU host.

VST3 and AU wrappers dynamically load the matching `.clap` from
`~/Library/Audio/Plug-Ins/CLAP/`. **You must install the CLAP zip
alongside the VST3/AU zip** — the wrappers are pure CLAP loaders, not
copies of the DSP.

Each plugin's display name includes a build number tag like
`SuperDuper Reverb [b75109]`. That confirms which build is loaded
without digging through plugin info.

---

## macOS (Apple Silicon)

> ⚠️ Intel Macs are not supported yet. Apple Silicon (M1/M2/M3/M4) only.
> macOS 12.0 (Monterey) or newer.

### 1. Download

From the latest [GitHub release](https://github.com/fortunto2/superduper-dsp/releases)
grab whatever formats you want:

- `superduper-dsp-<version>-macos-arm64.zip` — all 13 CLAP plugins (**always download this one**)
- `superduper-dsp-<version>-macos-arm64-vst3.zip` — all 13 VST3 wrappers (optional)
- `superduper-dsp-<version>-macos-arm64-au.zip` — all 13 Audio Unit wrappers (optional)

Or grab individual plugins (e.g. `SuperDuperReverb-<version>-macos-arm64-vst3.zip`)
if you only want a subset.

### 2. Unzip and install

Unzip each archive and drag the bundles into the matching system
folders. Create directories if they don't exist:

```bash
# CLAP — required for all formats
mkdir -p ~/Library/Audio/Plug-Ins/CLAP
mv SuperDuper*.clap ~/Library/Audio/Plug-Ins/CLAP/

# VST3 — optional
mkdir -p ~/Library/Audio/Plug-Ins/VST3
mv SuperDuper*.vst3 ~/Library/Audio/Plug-Ins/VST3/

# Audio Unit — optional
mkdir -p ~/Library/Audio/Plug-Ins/Components
mv SuperDuper*.component ~/Library/Audio/Plug-Ins/Components/
```

For an all-users install use `/Library/Audio/Plug-Ins/...` instead
(needs admin privileges, prefix `sudo` to the `mv` commands).

### 3. Bypass Gatekeeper quarantine

The bundles aren't notarised yet, so on first download macOS treats
them as "downloaded from the internet" and refuses to load them.
Strip the quarantine attribute in one shot:

```bash
xattr -dr com.apple.quarantine ~/Library/Audio/Plug-Ins/CLAP/SuperDuper*.clap
xattr -dr com.apple.quarantine ~/Library/Audio/Plug-Ins/VST3/SuperDuper*.vst3 2>/dev/null
xattr -dr com.apple.quarantine ~/Library/Audio/Plug-Ins/Components/SuperDuper*.component 2>/dev/null
```

(The `2>/dev/null` silences errors when a folder is empty because
you didn't install that format.)

### 4. Rescan in your DAW

- **REAPER**: Options → Preferences → Plug-ins → CLAP → **Clear cache and re-scan**.
  Same flow under VST3 if you installed those.
- **Bitwig Studio**: Settings → Locations → Plug-in Locations → **Rescan**.
- **Studio One**: Studio One → Options → Locations → VST Plug-ins → **Reset Blocklist** + **Locations** rescan.
- **Logic Pro / GarageBand**: launches the AU validator on next start —
  may take 30-60 s. Reopen the project.
- **DaVinci Resolve / Fusion**: Preferences → Audio → Plugin Path
  Rescan. If the plugin is rejected, see the Troubleshooting section.
- **Ableton Live 11+**: Preferences → Plug-ins → Use VST3 plug-in system folders → toggle ON, then **Rescan**.

### 5. Verify

The plugins should appear in the FX / instrument browser. Drop one
on a track:

- Effects (Reverb, Delay, Comp, …) — pass audio through, hear the effect.
- Instruments (Pad, Wave, Kubyz, Ambient) — Wave/Kubyz/Pad respond to MIDI; Ambient generates autonomously.
- Spectrum — pass-through analyser; the GUI shows live FFT.

If the wrapper format shows `[b…]` ID but produces silence, the most
common cause is that you forgot the CLAP zip — see step 2.

---

## Windows

> CLAP only for now. VST3 wrappers on Windows are plumbing-ready but
> not in CI yet — open a GitHub issue if you need them.

### 1. Download

`superduper-dsp-<version>-windows-x64.zip` from the
[releases page](https://github.com/fortunto2/superduper-dsp/releases).

### 2. Unzip and install

Unzip. Each `.clap` is a single file on Windows (not a directory).
Copy them into one of:

```
C:\Program Files\Common Files\CLAP\          (all users, needs admin)
%LOCALAPPDATA%\Programs\Common\CLAP\         (current user only)
```

### 3. Rescan

**REAPER**: Options → Preferences → Plug-ins → CLAP → **Clear cache and re-scan**.

---

## Linux

CLAP-only Linux builds aren't published yet. To build from source see
the [README build section](README.md#build-from-source). Drop the
resulting `.so` files into `~/.clap/` or `/usr/lib/clap/`.

---

## Uninstall

```bash
# macOS
rm -rf ~/Library/Audio/Plug-Ins/CLAP/SuperDuper*.clap
rm -rf ~/Library/Audio/Plug-Ins/VST3/SuperDuper*.vst3
rm -rf ~/Library/Audio/Plug-Ins/Components/SuperDuper*.component

# Windows
del "C:\Program Files\Common Files\CLAP\SuperDuper*.clap"
```

Then rescan in your DAW.

---

## Troubleshooting

**Wrapper VST3/AU shows up in the host but plays silence.**
The wrapper couldn't find its CLAP. Install the CLAP zip into
`~/Library/Audio/Plug-Ins/CLAP/` and rescan.

**REAPER doesn't see the plugin after install.**
Plug-ins panel → CLAP (or VST3) → Clear cache + Re-scan. If still
missing, verify the path is in the search list — REAPER scans the OS
standard paths by default but the list is editable.

**macOS says "plugin is damaged and can't be opened".**
You missed step 3 — run the `xattr` commands above and rescan.

**Logic Pro rejects the AU during validation.**
Open Terminal and run `auval -v aufx <subtype> SDsp` (or `aumu`
for instruments). The output explains exactly which validation step
failed. Most common: missing CLAP companion — see the silence case
above.

**Plugin loads but no audio (CLAP).**
Confirm the host track has stereo I/O. For Ambient/Pad/Wave/Kubyz
also confirm there's a MIDI source feeding it (or for Ambient, just
that the playback head is moving).

**GUI window is blank / crashes on open.**
Check `~/.superduper-dsp/<plugin>.log` for the last messages —
each plugin writes its own log file. Tail before opening the UI:

```bash
tail -F ~/.superduper-dsp/reverb.log
```

**Bypass plugin shows the host's automation lane recording random values.**
That's the legacy CC → param mapping for synths (Pad/Wave/Kubyz).
Disable CC mapping in the DAW's MIDI routing or block specific CCs.
Bug reports welcome — we may switch this to opt-in.

**REAPER cache problems after rebuild.**
Preferences → Plug-ins → CLAP → **Clear cache and re-scan**. The
build-number tag in the plugin name (`[b…]`) tells you which build
is loaded — if it doesn't update, the cache is stale.
