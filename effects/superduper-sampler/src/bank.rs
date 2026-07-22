//! Sample bank — scans known folders for WAV files and lazily
//! decodes them on demand. The scan + decode happen on the main /
//! GUI thread; the audio thread only reads through an `Arc<SampleData>`
//! that gets atomically swapped when the user picks a different sample.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use superduper_synth_core::wav::{parse_wav_file, WavData};

/// Decoded sample ready to be played. Wrapped in an Arc on the audio
/// thread side so a swap is one pointer write — no large copy.
pub struct SampleData {
    pub display_name: String,
    pub source_path: PathBuf,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,   // interleaved
    /// Pre-computed peak envelope for waveform display, one (min,
    /// max) entry per `PEAK_BUCKETS` slot across the whole sample.
    /// Cheap to render at any GUI width by sampling a uniform grid
    /// out of this fixed-size buffer.
    pub peaks: Vec<(f32, f32)>,
    /// Detected fundamental frequency in Hz, if pitch could be
    /// identified. None for noise / inharmonic / silent samples.
    /// Used by the GUI tuner so the user can see what note the
    /// sample sits on without ear-tuning by hand.
    pub detected_pitch_hz: Option<f32>,
}

/// Number of peak buckets we cache per sample. 1024 is plenty for
/// any sensible GUI width (Sampler default is ~640 px wide) and
/// fits in 8 KB of memory.
pub const PEAK_BUCKETS: usize = 1024;

impl SampleData {
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 { 0 } else { self.samples.len() / self.channels as usize }
    }
    /// Stereo read with linear interpolation at a fractional frame
    /// position. Returns silence past the end.
    ///
    /// Bug fix 2026-05: the prior version returned silence at
    /// `i + 1 >= total`. That cost the very last frame on forward
    /// playback (barely audible), but in REVERSE playback the FIRST
    /// read happens at trim_hi - 1 which is exactly that edge case,
    /// so reverse started with a click of silence. The "last frame"
    /// branch now returns the actual last sample (no interpolation
    /// because there's no neighbour to lerp to).
    #[inline]
    pub fn read_stereo_lerp(&self, frame_pos: f64) -> (f32, f32) {
        let total = self.frame_count();
        if total == 0 { return (0.0, 0.0); }
        let i = frame_pos.floor() as usize;
        if i >= total { return (0.0, 0.0); }
        let ch = self.channels as usize;
        let base = i * ch;
        if i + 1 >= total {
            // Last frame: no neighbour to interpolate with, return as-is.
            if ch == 1 {
                let a = self.samples[base];
                return (a, a);
            }
            return (self.samples[base], self.samples[base + 1]);
        }
        let frac = (frame_pos - i as f64) as f32;
        let (l0, r0, l1, r1) = if ch == 1 {
            let a = self.samples[base];
            let b = self.samples[base + 1];
            (a, a, b, b)
        } else {
            (
                self.samples[base],
                self.samples[base + 1],
                self.samples[base + ch],
                self.samples[base + ch + 1],
            )
        };
        let l = l0 + (l1 - l0) * frac;
        let r = r0 + (r1 - r0) * frac;
        (l, r)
    }
}

/// Empty placeholder used by the audio thread before any sample is
/// loaded. Plays silence — same shape as a real SampleData so we
/// don't need an Option around the Arc.
pub fn empty_sample() -> Arc<SampleData> {
    Arc::new(SampleData {
        display_name: "(no sample)".into(),
        source_path: PathBuf::new(),
        sample_rate: 48_000,
        channels: 1,
        samples: Vec::new(),
        peaks: Vec::new(),
        detected_pitch_hz: None,
    })
}

/// Detect the fundamental frequency of an interleaved sample using the
/// shared YIN detector in `synth_core::pitch`. Runs once at load time on
/// the GUI thread (not RT-safe — it allocates a mono scratch buffer).
/// Returns None for noise / silence / inharmonic content.
///
/// Thin wrapper: sum the interleaved channels down to a mono buffer, then
/// hand off to `synth_core::pitch::detect_pitch_hz`, which picks the
/// loudest non-silent slice (so 808s with long silent tails don't drown
/// out the kick body) and searches periods from 30 Hz to 4 kHz — the same
/// algorithm the vocoder's carrier tracker uses, kept in one place.
pub fn detect_pitch_hz(samples: &[f32], channels: u16, sample_rate: u32) -> Option<f32> {
    let ch = channels.max(1) as usize;
    let frame_count = samples.len() / ch;
    if frame_count < 2048 {
        return None;
    }
    let mono: Vec<f32> = if ch == 1 {
        samples.to_vec()
    } else {
        (0..frame_count)
            .map(|f| {
                let base = f * ch;
                let sum: f32 = (0..ch).map(|c| samples[base + c]).sum();
                sum / ch as f32
            })
            .collect()
    };
    superduper_synth_core::pitch::detect_pitch_hz(&mono, sample_rate)
}

/// Pre-compute a coarse peak envelope for the GUI waveform display.
/// Returns PEAK_BUCKETS pairs (min, max) — each bucket covers an
/// equal slice of the source. For mono samples we use the channel
/// directly; for stereo we take the bigger absolute peak across the
/// two channels so the curve always reads as the "loudest part".
fn compute_peaks(samples: &[f32], channels: u16) -> Vec<(f32, f32)> {
    let frame_count = if channels == 0 { 0 } else { samples.len() / channels as usize };
    if frame_count == 0 { return Vec::new(); }
    let ch = channels as usize;
    let mut out = Vec::with_capacity(PEAK_BUCKETS);
    for b in 0..PEAK_BUCKETS {
        let lo = (b * frame_count) / PEAK_BUCKETS;
        let hi = ((b + 1) * frame_count) / PEAK_BUCKETS;
        let hi = hi.max(lo + 1).min(frame_count);
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for f in lo..hi {
            let base = f * ch;
            // Combine channels into a peak-aware mono envelope.
            let s = if ch == 1 { samples[base] }
                else { samples[base].abs().max(samples[base + 1].abs())
                       * samples[base].signum() };
            if s < min { min = s; }
            if s > max { max = s; }
        }
        if !min.is_finite() { min = 0.0; }
        if !max.is_finite() { max = 0.0; }
        out.push((min, max));
    }
    out
}

/// Built-in fallback folder set, used when the user has no
/// `sampler-config.json` yet (first launch).
pub fn default_sample_folders() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut paths = Vec::new();
    if !home.as_os_str().is_empty() {
        paths.push(home.join("Music/SuperDuper Samples"));
        paths.push(home.join("Music/Favorite 808s"));
    }
    paths
}

/// Where we keep the user's editable folder list.
pub fn config_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".superduper-dsp").join("sampler-config.json")
}

/// Load the user's sample-folder list from disk, falling back to the
/// built-in defaults when the file is missing or empty. The format
/// is a tiny hand-rolled JSON `{"sample_roots": ["path1", "path2"]}`
/// — no serde dep needed.
pub fn load_folders_config() -> Vec<PathBuf> {
    let path = config_path();
    let Ok(bytes) = std::fs::read_to_string(&path) else {
        return default_sample_folders();
    };
    // Cheap extraction: find every quoted string inside the file,
    // each one is a folder. Robust enough for our flat JSON shape
    // and saves pulling serde for a 10-line config.
    let mut out = Vec::new();
    let mut in_str = false;
    let mut buf = String::new();
    for c in bytes.chars() {
        if c == '"' {
            if in_str {
                if buf != "sample_roots" && !buf.is_empty() {
                    out.push(PathBuf::from(&buf));
                }
                buf.clear();
                in_str = false;
            } else { in_str = true; }
        } else if in_str {
            buf.push(c);
        }
    }
    if out.is_empty() { default_sample_folders() } else { out }
}

/// Persist the folder list back to disk. Creates `~/.superduper-dsp/`
/// if needed. Best-effort — failures log via the caller, no crash.
pub fn save_folders_config(roots: &[PathBuf]) -> Result<(), std::io::Error> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut json = String::from("{\n  \"sample_roots\": [\n");
    for (i, p) in roots.iter().enumerate() {
        let s = p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
        json.push_str(&format!("    \"{}\"{}\n",
            s, if i + 1 < roots.len() { "," } else { "" }));
    }
    json.push_str("  ]\n}\n");
    std::fs::write(path, json)
}

/// A WAV file tagged with the "pack" it came from. Pack is the first
/// subfolder of the root scan dir (so `~/Music/SuperDuper Samples/TR-808/kick.wav`
/// has pack "TR-808"). Lets the GUI show a two-level Pack → Sample
/// picker so a folder full of 808 packs doesn't drown the dropdown.
#[derive(Clone)]
pub struct PackedSample {
    pub pack: String,
    pub path: PathBuf,
}

/// Scan the configured folders recursively and return a sorted list
/// of every WAV file found, each tagged with its pack (first
/// subfolder beneath the root, or the root folder's own name when
/// the file sits at the root level).
pub fn scan_folders(roots: &[PathBuf]) -> Vec<PackedSample> {
    let mut out = Vec::new();
    for root in roots {
        if !root.exists() { continue; }
        // Files that live directly in the root get the root's name
        // as their pack (e.g. "Favorite 808s"). Files in a subdir
        // get that subdir's name. Two-level deeper stays bundled
        // with the first subdir.
        let root_name = root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        scan_recursive_tagged(root, &root_name, &root_name, &mut out, 0);
    }
    out.sort_by(|a, b| (a.pack.as_str(), a.path.as_path()).cmp(&(b.pack.as_str(), b.path.as_path())));
    out
}

/// Walk the directory tree tagging every WAV with `current_pack`.
/// When we descend into a *first-level* subdir of the root we switch
/// the pack to that subdir's name; deeper levels keep their parent's
/// pack tag so e.g. `TR-808/Kicks/long-808.wav` is bucketed as
/// "TR-808" regardless of the Kicks/ split.
fn scan_recursive_tagged(
    dir: &Path,
    root_pack: &str,
    current_pack: &str,
    out: &mut Vec<PackedSample>,
    depth: u32,
) {
    if depth > 4 { return; }
    let Ok(entries) = std::fs::read_dir(dir) else { return; };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            // Only the *first* descent below root flips the pack tag;
            // deeper folders keep their grandparent's pack.
            let new_pack = if depth == 0 { name.as_str() } else { current_pack };
            scan_recursive_tagged(&path, root_pack, new_pack, out, depth + 1);
        } else if path.extension().map_or(false, |e| {
            let e = e.to_string_lossy().to_lowercase();
            e == "wav" || e == "wave"
        }) {
            out.push(PackedSample { pack: current_pack.to_string(), path });
        }
    }
}

/// Decode a WAV file into a SampleData. Uses the file stem as the
/// display name shown in the GUI dropdown.
pub fn load_sample(path: &Path) -> Result<SampleData, String> {
    let wav: WavData = parse_wav_file(path).map_err(|e| e.to_string())?;
    let display_name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let peaks = compute_peaks(&wav.samples, wav.channels);
    let detected_pitch_hz = detect_pitch_hz(&wav.samples, wav.channels, wav.sample_rate);
    Ok(SampleData {
        display_name,
        source_path: path.to_path_buf(),
        sample_rate: wav.sample_rate,
        channels: wav.channels,
        samples: wav.samples,
        peaks,
        detected_pitch_hz,
    })
}

/// Convert a frequency in Hz to the nearest MIDI note name and cents
/// offset. e.g. 442 Hz → ("A4", +8). Returns ("—", 0) for silence /
/// unknown.
pub fn pitch_to_note_name(hz: f32) -> (String, i32) {
    if !hz.is_finite() || hz <= 0.0 { return ("—".into(), 0); }
    let midi_f = 69.0 + 12.0 * (hz / 440.0).log2();
    let nearest = midi_f.round() as i32;
    let cents = ((midi_f - nearest as f32) * 100.0).round() as i32;
    let nn = nearest.clamp(0, 127);
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let octave = nn / 12 - 1;
    let pc = (nn % 12) as usize;
    (format!("{}{}", NAMES[pc], octave), cents)
}
