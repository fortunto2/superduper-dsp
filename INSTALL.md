# Installing SuperDuper DSP plugins

Three CLAP plugins ship from this repo:

- **SuperDuper Reverb** — Dattorro plate reverb with sidechain ducking
- **SuperDuper Supermass** — Valhalla-style cascade reverb (chorus + double tail)
- **SuperDuper Spectrum** — pass-through analyzer with spectrum / spectrogram / split views

They run in any CLAP-aware DAW: REAPER 7+, Bitwig Studio 4.3+, Studio One 6.5+,
FL Studio 21+, MultitrackStudio, etc.

---

## macOS (Apple Silicon)

> ⚠️ Intel Macs are not supported yet. Apple Silicon (M1/M2/M3/M4) only.

### 1. Download

From the latest [GitHub release](https://github.com/fortunto2/superduper-dsp/releases)
grab one of:

- `superduper-dsp-<version>-macos-arm64.zip` — all three plugins
- or individual `SuperDuperReverb-…zip`, `SuperDuperSupermass-…zip`,
  `SuperDuperSpectrum-…zip`

### 2. Unzip and install

Unzip and drag the `.clap` bundles into:

```
~/Library/Audio/Plug-Ins/CLAP/
```

(Create the directory if it doesn't exist. `~` is your home folder.)

For a single all-users install, use `/Library/Audio/Plug-Ins/CLAP/` instead —
you'll need admin privileges.

### 3. Bypass Gatekeeper quarantine

The build isn't notarized yet, so the first time macOS sees the bundles it
treats them as "downloaded from the internet" and refuses to load them.
Strip the quarantine attribute with one command:

```bash
xattr -dr com.apple.quarantine ~/Library/Audio/Plug-Ins/CLAP/SuperDuper*.clap
```

(If you used `/Library/Audio/Plug-Ins/CLAP/`, prefix that with `sudo`.)

### 4. Rescan in your DAW

**REAPER**: Options → Preferences → Plug-ins → CLAP → **Clear cache and re-scan**.

**Bitwig / Studio One**: rescan plugin paths in the plugin preferences pane.

You should now see `SuperDuper Reverb`, `SuperDuper Supermass`, and
`SuperDuper Spectrum` in the FX browser.

### 5. Verify

Drop each plugin on a track:
- Reverb / Supermass — give them some audio and hear the tail.
- Spectrum — pass-through, but the GUI shows live FFT or spectrogram.

The plugin display name in your DAW includes a build number like
`SuperDuper Reverb [b75109]`. That's there so you can confirm at a glance
which build is loaded.

---

## Windows

> 🚧 Windows builds are produced by CI per release. Look for
> `*-windows-x64.zip` in the latest release.

### 1. Download

`superduper-dsp-<version>-windows-x64.zip` from the
[releases page](https://github.com/fortunto2/superduper-dsp/releases).

### 2. Unzip and install

Unzip. Each `.clap` is a single file on Windows (not a directory).
Copy them to:

```
C:\Program Files\Common Files\CLAP\
```

(System-wide; requires admin. Create the folder if it doesn't exist.)

Per-user alternative:

```
%LOCALAPPDATA%\Programs\Common\CLAP\
```

### 3. Rescan

**REAPER**: Options → Preferences → Plug-ins → CLAP → **Clear cache and re-scan**.

---

## Linux

Linux builds aren't published yet. To build from source see the
[README](README.md) — it's `cargo build --release -p superduper-reverb` etc.
plus copying the resulting `.so` into `~/.clap/`.

---

## Uninstall

Delete the `.clap` bundles from the install directory and rescan in your DAW.
On macOS:

```bash
rm -rf ~/Library/Audio/Plug-Ins/CLAP/SuperDuper*.clap
```

## Troubleshooting

**REAPER doesn't see the plugin after install.**
Plug-ins panel → CLAP → Clear cache + Re-scan. If still missing, make sure
the path is included in the CLAP search list (REAPER scans the OS standard
paths by default, but verify).

**Plugin loads but no audio.**
Confirm REAPER's track has stereo I/O (right-click track → Track Channels).

**GUI window is blank / crashes on open.**
Check `~/.superduper-dsp/<plugin>.log` for last messages. Tail it before
opening the FX UI: `tail -F ~/.superduper-dsp/reverb.log`.

**macOS says "plugin is damaged and can't be opened".**
You missed step 3 — run the `xattr` command above and rescan.
