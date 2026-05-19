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
}

impl SampleData {
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 { 0 } else { self.samples.len() / self.channels as usize }
    }
    /// Stereo read with linear interpolation at a fractional frame
    /// position. Returns silence past the end.
    #[inline]
    pub fn read_stereo_lerp(&self, frame_pos: f64) -> (f32, f32) {
        let total = self.frame_count();
        if total == 0 { return (0.0, 0.0); }
        let i = frame_pos.floor() as usize;
        if i + 1 >= total { return (0.0, 0.0); }
        let frac = (frame_pos - i as f64) as f32;
        let ch = self.channels as usize;
        let base = i * ch;
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
    })
}

/// Folder set the plugin scans on activate. User can drop WAVs into
/// any of these and they show up in the GUI dropdown. Falls back
/// gracefully when a path doesn't exist.
pub fn default_sample_folders() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let mut paths = Vec::new();
    if !home.as_os_str().is_empty() {
        paths.push(home.join("Music/SuperDuper Samples"));
        paths.push(home.join("Music/Favorite 808s"));
    }
    paths
}

/// Scan the configured folders recursively and return a sorted list
/// of every WAV file found. Result is cheap to call (<10 ms for a
/// few hundred entries) so it's fine on the GUI thread.
pub fn scan_folders(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        scan_recursive(root, &mut out, 0);
    }
    out.sort();
    out
}

fn scan_recursive(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) {
    if depth > 4 { return; }  // cap walk depth
    let Ok(entries) = std::fs::read_dir(dir) else { return; };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_recursive(&path, out, depth + 1);
        } else if path.extension().map_or(false, |e| {
            let e = e.to_string_lossy().to_lowercase();
            e == "wav" || e == "wave"
        }) {
            out.push(path);
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
    Ok(SampleData {
        display_name,
        source_path: path.to_path_buf(),
        sample_rate: wav.sample_rate,
        channels: wav.channels,
        samples: wav.samples,
    })
}
