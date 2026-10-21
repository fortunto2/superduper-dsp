//! Drum one-shot library: what a sample is (kick, snare, hat…) from its name
//! and, when the name says nothing (`909-BT0A0A7.WAV`), from its sound.
//!
//! Pure classification lives here; scanning folders and decoding audio sit
//! behind `sfz-load` (`scan_library`) because they need the decoder.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum DrumClass {
    Kick,
    Snare,
    Clap,
    HatClosed,
    HatOpen,
    Cymbal,
    Tom,
    Perc,
}

impl DrumClass {
    pub const ALL: [DrumClass; 8] = [
        DrumClass::Kick, DrumClass::Snare, DrumClass::Clap, DrumClass::HatClosed,
        DrumClass::HatOpen, DrumClass::Cymbal, DrumClass::Tom, DrumClass::Perc,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DrumClass::Kick => "kick",
            DrumClass::Snare => "snare",
            DrumClass::Clap => "clap",
            DrumClass::HatClosed => "hat closed",
            DrumClass::HatOpen => "hat open",
            DrumClass::Cymbal => "cymbal",
            DrumClass::Tom => "tom",
            DrumClass::Perc => "perc",
        }
    }

    /// Which SuperDuper Drum pad browses this class (Kick, Snare, HHc, HHo,
    /// Clap, Cowbell/perc). Cymbals and toms share the perc pad for now.
    pub fn pad(self) -> usize {
        match self {
            DrumClass::Kick => 0,
            DrumClass::Snare => 1,
            DrumClass::HatClosed => 2,
            DrumClass::HatOpen | DrumClass::Cymbal => 3,
            DrumClass::Clap => 4,
            DrumClass::Tom | DrumClass::Perc => 5,
        }
    }
}

/// Lower-case alphanumeric tokens, splitting camelCase-free names on any
/// separator. `KSHMR_Closed_Hat_03` → [kshmr, closed, hat, 03].
fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

/// Two passes: whole words first, then the two-letter machine codes
/// (`bd`, `sd`, `st`, `cp`…). One pass let `KSHMR_x_SD_Madness_Kick` — an
/// artist tag and a kick — land on the snare pad, and a generic `sub` pulled
/// `Sub_Drop` risers and `VO_Sub` vocals in as kicks; both measured on KSHMR.
fn class_from_tokens(t: &[String]) -> Option<DrumClass> {
    let has = |w: &str| t.iter().any(|x| x == w);
    let starts = |w: &str| t.iter().any(|x| x.starts_with(w));
    let open = has("open") || has("opened");

    // Pass 1 — words.
    if open && (starts("hat") || starts("hihat")) { return Some(DrumClass::HatOpen); }
    if starts("hat") || starts("hihat") { return Some(DrumClass::HatClosed); }
    if starts("crash") || starts("ride") || starts("cymbal") || starts("splash") || starts("china") {
        return Some(DrumClass::Cymbal);
    }
    if starts("kick") || starts("kik") || starts("bassdrum") || starts("808") || starts("rust808") {
        return Some(DrumClass::Kick);
    }
    if starts("snare") || starts("snr") { return Some(DrumClass::Snare); }
    if starts("clap") || starts("handclp") || starts("snap") { return Some(DrumClass::Clap); }
    if starts("tom") { return Some(DrumClass::Tom); }
    if starts("rim") || starts("perc") || starts("shaker") || starts("cowbell") || starts("conga") || starts("bongo")
        || starts("tamb") || starts("clave") || starts("woodblock") || starts("timbal") || starts("guiro")
        || starts("cabasa") || starts("triangle") || has("block") || has("bell") || has("tick")
    {
        return Some(DrumClass::Perc);
    }

    // Pass 2 — machine codes (TR-909 `BT`/`ST`/`HHCD`…, 808 `BD`/`CP`/`CL`…).
    if has("ohh") || has("oh") || starts("hhod") || starts("hho") || starts("opcl") { return Some(DrumClass::HatOpen); }
    if has("hh") || has("ch") || has("chh") || starts("hhcd") || starts("hhc") { return Some(DrumClass::HatClosed); }
    if starts("cshd") || starts("cym") { return Some(DrumClass::Cymbal); }
    if has("bd") || has("bt") || starts("btaa") { return Some(DrumClass::Kick); }
    if has("sd") || has("st") || starts("stat") { return Some(DrumClass::Snare); }
    if has("cp") || starts("clop") { return Some(DrumClass::Clap); }
    if ["ht", "mt", "lt", "htad", "mtad", "ltad", "htada", "mtada", "ltada"].iter().any(|w| has(w)) {
        return Some(DrumClass::Tom);
    }
    if has("cl") || has("ma") || has("rs") { return Some(DrumClass::Perc); }
    None
}

/// Tokens glued to a code, e.g. Cassette808 `BD01` → `bd`, 909 `BT0A0A7` → `bt`.
fn alpha_prefixes(t: &[String]) -> Vec<String> {
    t.iter()
        .map(|x| x.chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>())
        .filter(|x| !x.is_empty())
        .collect()
}

/// Class from the file name first, then its parent folders (KSHMR keeps the
/// type in the folder: `KSHMR_Kicks/KSHMR_Big_Kick_01_F.wav`).
pub fn classify_name(path: &Path) -> Option<DrumClass> {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let t = tokens(&stem);
    if let Some(c) = class_from_tokens(&t).or_else(|| class_from_tokens(&alpha_prefixes(&t))) {
        return Some(c);
    }
    for dir in path.ancestors().skip(1).take(2) {
        let name = dir.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if let Some(c) = class_from_tokens(&tokens(&name)) { return Some(c); }
    }
    None
}

/// Loops, fills and song starters are not one-shots: a pad should never
/// offer them. Decided from names; `scan_library` also drops anything over
/// 4 s.
pub fn looks_like_loop(path: &Path) -> bool {
    let s = path.to_string_lossy().to_lowercase();
    ["loop", "fill", "starter", "groove", "break", "bpm"].iter().any(|w| s.contains(w))
}

/// Pitch class (0 = C) from a trailing key token: `Kick_02_D#`, `808_Gm`, `Sub Cs1`.
pub fn key_from_name(path: &Path) -> Option<u8> {
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let last = stem.split(|c: char| c == '_' || c == ' ' || c == '-').filter(|t| !t.is_empty()).next_back()?;
    let lower = last.to_lowercase();
    let mut ch = lower.chars();
    let base = match ch.next()? { 'c' => 0, 'd' => 2, 'e' => 4, 'f' => 5, 'g' => 7, 'a' => 9, 'b' => 11, _ => return None };
    let rest: String = ch.collect();
    let (acc, tail) = if let Some(r) = rest.strip_prefix('#') { (1, r.to_string()) }
        else if let Some(r) = rest.strip_prefix('s') { (1, r.to_string()) }
        else if rest.starts_with('b') && rest.len() <= 2 { (-1, rest[1..].to_string()) }
        else { (0, rest.clone()) };
    // Allowed tails: nothing, "m" (minor), an octave digit, or m + digit.
    let tail = tail.strip_prefix('m').unwrap_or(&tail);
    if !(tail.is_empty() || (tail.len() == 1 && tail.chars().all(|c| c.is_ascii_digit()))) { return None; }
    Some(((base + acc + 12) % 12) as u8)
}

/// What the sound-based classifier looks at.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AudioFeatures {
    /// Seconds until the envelope last falls below −40 dB of its peak.
    pub duration_s: f32,
    /// Share of energy below ~150 Hz, first 300 ms.
    pub low_ratio: f32,
    /// Spectral centroid of the attack (first 4096 samples after onset), Hz.
    pub centroid_hz: f32,
    /// Spectral flatness of the attack: 0 = pure tone, 1 = white noise.
    pub flatness: f32,
    /// Fundamental of the tail (`drum_pitch_hz`), when there is one.
    #[serde(default)]
    pub pitch_hz: Option<f32>,
}

/// Mono samples at `sr` → features. Allocates (FFT); offline only.
pub fn audio_features(mono: &[f32], sr: f32) -> AudioFeatures {
    let peak = mono.iter().fold(0f32, |m, &x| m.max(x.abs())).max(1e-9);
    let onset = mono.iter().position(|&x| x.abs() > peak * 0.1).unwrap_or(0);
    // Envelope: 5 ms windows of peak level.
    let win = ((sr * 0.005) as usize).max(1);
    let floor = peak * 0.01; // −40 dB
    let last_loud = mono.chunks(win).rposition(|c| c.iter().any(|&x| x.abs() > floor)).unwrap_or(0);
    let duration_s = ((last_loud + 1) * win).saturating_sub(onset) as f32 / sr;

    // Low-band share: one-pole LP at 150 Hz, energy ratio over 300 ms.
    let n_low = ((sr * 0.3) as usize).min(mono.len().saturating_sub(onset));
    let a = (-2.0 * std::f32::consts::PI * 150.0 / sr).exp();
    let (mut lp, mut e_low, mut e_all) = (0f32, 0f64, 1e-12f64);
    for &x in &mono[onset..onset + n_low] {
        lp = a * lp + (1.0 - a) * x;
        e_low += (lp * lp) as f64;
        e_all += (x * x) as f64;
    }
    let low_ratio = (e_low / e_all).min(1.0) as f32;

    // Attack spectrum.
    const N: usize = 4096;
    let mut buf = vec![0f32; N];
    for (i, slot) in buf.iter_mut().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (N - 1) as f32).cos();
        *slot = mono.get(onset + i).copied().unwrap_or(0.0) * w;
    }
    let mut planner = realfft::RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N);
    let mut spec = fft.make_output_vec();
    let _ = fft.process(&mut buf, &mut spec);
    let mags: Vec<f64> = spec.iter().map(|c| c.norm() as f64 + 1e-12).collect();
    let bin_hz = sr as f64 / N as f64;
    let total: f64 = mags.iter().sum();
    let centroid_hz = (mags.iter().enumerate().map(|(i, m)| i as f64 * bin_hz * m).sum::<f64>() / total) as f32;
    // Flatness over 100 Hz..16 kHz: geometric / arithmetic mean of power.
    let lo = (100.0 / bin_hz) as usize;
    let hi = ((16_000.0 / bin_hz) as usize).min(mags.len() - 1);
    let band = &mags[lo..hi.max(lo + 1)];
    let ln_mean = band.iter().map(|m| (m * m).ln()).sum::<f64>() / band.len() as f64;
    let mean = band.iter().map(|m| m * m).sum::<f64>() / band.len() as f64;
    let flatness = (ln_mean.exp() / mean).clamp(0.0, 1.0) as f32;
    AudioFeatures { duration_s, low_ratio, centroid_hz, flatness, pitch_hz: drum_pitch_hz(mono, sr) }
}

/// The pitch a drum is heard at: the strongest spectral peak (30-600 Hz) of
/// its TAIL, 60-460 ms after the peak. A kick's attack sweeps down from
/// several times its note, so the loudest slice — what `pitch::detect_pitch_hz`
/// reads — names the sweep, not the note. Measured on 273 KSHMR kicks and
/// toms whose names carry the key (examples/drum_key_probe.rs): loudest-slice
/// YIN 18 % exact pitch class, this 66 % exact / 83 % within a semitone.
pub fn drum_pitch_hz(mono: &[f32], sr: f32) -> Option<f32> {
    let peak_i = mono.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).map(|x| x.0)?;
    let start = (peak_i + (0.06 * sr) as usize).min(mono.len());
    let end = (start + (0.40 * sr) as usize).min(mono.len());
    let x = &mono[start..end];
    if x.len() < 1024 { return None; }
    const N: usize = 32768;
    let mut buf = vec![0f32; N];
    for (i, v) in x.iter().enumerate().take(N) {
        let w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (x.len() - 1) as f32).cos();
        buf[i] = v * w;
    }
    let mut planner = realfft::RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N);
    let mut spec = fft.make_output_vec();
    fft.process(&mut buf, &mut spec).ok()?;
    let bin = sr / N as f32;
    let (lo, hi) = ((30.0 / bin) as usize, ((600.0 / bin) as usize).min(spec.len() - 2));
    let m: Vec<f32> = spec.iter().map(|c| c.norm()).collect();
    let k = (lo.max(1)..hi).max_by(|&a, &b| m[a].total_cmp(&m[b]))?;
    // Something has to be there: a hat's tail is noise, not a note.
    let floor = m[lo..hi].iter().sum::<f32>() / (hi - lo) as f32;
    if m[k] < 1e-6 || m[k] < floor * 8.0 { return None; }
    let (a, b, c) = (m[k - 1].max(1e-12).ln(), m[k].ln(), m[k + 1].max(1e-12).ln());
    let den = a - 2.0 * b + c;
    let d = if den.abs() > 1e-9 { 0.5 * (a - c) / den } else { 0.0 };
    Some((k as f32 + d) * bin)
}

/// Pitch class (0 = C) of a frequency.
pub fn pitch_class(hz: f32) -> u8 {
    (((12.0 * (hz / 440.0).log2()).round() as i32 + 9).rem_euclid(12)) as u8
}

/// The shift (semitones, −6..=5) that takes `from` to `to` by the shortest way.
pub fn key_shift(from: u8, to: u8) -> i32 {
    let d = (to as i32 - from as i32).rem_euclid(12);
    if d > 5 { d - 12 } else { d }
}

/// The song-key bus: one small text file any tool, agent or plugin can write
/// ("A", "F#m", "Bb major") and SuperDuper Drum's Key = Auto reads. A file,
/// because every plugin is its own dylib — they share no statics.
pub fn song_key_path() -> PathBuf {
    std::env::var_os("SUPERDUPER_SONG_KEY").map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".superduper-dsp").join("song-key")
    })
}

/// Root pitch class of a key name: "A", "f#m", "Bb major", "C# minor".
pub fn parse_key_name(s: &str) -> Option<u8> {
    let t = s.trim();
    let mut ch = t.chars();
    let base = match ch.next()?.to_ascii_lowercase() { 'c' => 0, 'd' => 2, 'e' => 4, 'f' => 5, 'g' => 7, 'a' => 9, 'b' => 11, _ => return None };
    let acc = match ch.next() { Some('#') | Some('♯') => 1, Some('b') | Some('♭') => -1, _ => 0 };
    Some(((base + acc + 12) % 12) as u8)
}

/// Classes that carry a note worth tuning: kicks/808s and toms. Snares,
/// hats, claps and cymbals are left alone.
pub fn is_tonal(c: DrumClass) -> bool { matches!(c, DrumClass::Kick | DrumClass::Tom) }

/// Rule-based classifier. Thresholds were set on KSHMR Vol.3 (labelled by
/// folder) and checked on TR-909 + Cassette 808 (labelled by name) — the
/// numbers are in `examples/drum_classify.rs`.
pub fn classify_audio(f: &AudioFeatures) -> DrumClass {
    if f.low_ratio > 0.45 && f.centroid_hz < 1500.0 {
        return DrumClass::Kick;
    }
    if f.centroid_hz > 5000.0 && f.flatness > 0.2 {
        return if f.duration_s < 0.3 { DrumClass::HatClosed }
            else if f.duration_s < 1.2 { DrumClass::HatOpen }
            else { DrumClass::Cymbal };
    }
    if f.low_ratio > 0.2 && f.flatness < 0.15 {
        return DrumClass::Tom;
    }
    if f.flatness > 0.15 && f.centroid_hz > 1500.0 {
        return DrumClass::Snare;
    }
    DrumClass::Perc
}

/// How a library entry's class was decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum ClassSource { Name, Audio }

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DrumSample {
    pub path: PathBuf,
    /// Top folder under the scan root.
    pub pack: String,
    pub name: String,
    pub class: DrumClass,
    pub source: ClassSource,
    /// Pitch class: from the name when it carries one, else from the sound
    /// for tonal classes (`key_guessed`).
    pub key: Option<u8>,
    #[serde(default)]
    pub key_guessed: bool,
    pub duration_s: f32,
}

/// One playing one-shot: a sample at a pitch, with an exponential fade whose
/// time constant follows the pad's Decay. RT-safe; the `Arc` it holds is a
/// clone, never the last reference (the pad slot or the retired list keeps
/// one), so dropping it here never frees sample memory on the audio thread.
#[derive(Debug, Default)]
pub struct SampleHit {
    sample: Option<std::sync::Arc<crate::sfz::SampleBuf>>,
    pos: f64,
    step: f64,
    gain: f32,
    env: f32,
    coef: f32,
    choke_left: u32,
    choke_total: u32,
}

/// Decay knob → fade time constant. ×6 keeps a sample at the pad's default
/// Decay close to its natural length (kick 0.42 s → τ 2.5 s, −1.7 dB at
/// 0.5 s) while the bottom of the range shortens it hard.
pub const SAMPLE_DECAY_SCALE: f32 = 6.0;

impl SampleHit {
    pub fn is_active(&self) -> bool { self.sample.is_some() }

    pub fn start(&mut self, s: std::sync::Arc<crate::sfz::SampleBuf>, host_sr: f32, tune_st: f32, decay_s: f32, gain: f32) {
        self.step = 2f64.powf(tune_st as f64 / 12.0) * s.sample_rate as f64 / host_sr as f64;
        self.pos = 0.0;
        self.gain = gain;
        self.env = 1.0;
        let tau = (decay_s * SAMPLE_DECAY_SCALE).max(0.005);
        self.coef = (-1.0 / (tau * host_sr)).exp();
        self.choke_left = 0;
        self.choke_total = 0;
        self.sample = Some(s);
    }

    /// Fade out over ~5 ms (hat choke, retrigger of a busy slot).
    pub fn choke(&mut self, host_sr: f32) {
        if self.sample.is_some() && self.choke_left == 0 {
            let n = ((host_sr * 0.005) as u32).max(8);
            self.choke_left = n;
            self.choke_total = n;
        }
    }

    #[inline]
    pub fn process(&mut self) -> (f32, f32) {
        let (frames, a0, b0, a1, b1) = {
            let Some(s) = self.sample.as_ref() else { return (0.0, 0.0) };
            let i = self.pos.floor() as isize;
            let (a0, b0) = s.frame(i);
            let (a1, b1) = s.frame(i + 1);
            (s.frames, a0, b0, a1, b1)
        };
        let t = (self.pos - self.pos.floor()) as f32;
        let mut g = self.gain * self.env;
        let mut done = false;
        if self.choke_left > 0 {
            g *= self.choke_left as f32 / self.choke_total.max(1) as f32;
            self.choke_left -= 1;
            done = self.choke_left == 0;
        }
        let out = ((a0 + (a1 - a0) * t) * g, (b0 + (b1 - b0) * t) * g);
        self.env *= self.coef;
        self.pos += self.step;
        if done || self.pos >= frames as f64 || self.env < 1e-4 { self.sample = None; }
        out
    }
}

#[cfg(feature = "sfz-load")]
pub use scan::*;

#[cfg(feature = "sfz-load")]
mod scan {
    use super::*;
    use crate::sfz::load::decode_file;
    use std::collections::HashMap;

    /// Longest one-shot a pad will offer.
    pub const MAX_ONESHOT_S: f32 = 4.0;

    #[derive(serde::Serialize, serde::Deserialize, Default)]
    struct Cache {
        version: u32,
        /// path → (size, mtime secs, features)
        entries: HashMap<PathBuf, (u64, u64, AudioFeatures)>,
    }
    const CACHE_VERSION: u32 = 2; // 2: features carry pitch_hz

    fn cache_path() -> PathBuf {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(".superduper-dsp").join("drum-library.json")
    }

    fn stamp(p: &Path) -> Option<(u64, u64)> {
        let m = std::fs::metadata(p).ok()?;
        let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
        Some((m.len(), t))
    }

    pub fn mono_of(buf: &crate::sfz::SampleBuf) -> Vec<f32> {
        let c = buf.channels.max(1);
        (0..buf.frames)
            .map(|i| (0..c).map(|ch| buf.data.get(i * c + ch).copied().unwrap_or(0) as f32).sum::<f32>() / (c as f32 * 32768.0))
            .collect()
    }

    /// Default roots: our sample folders plus any mounted volume's
    /// `SuperDuper Samples` (the external drive).
    pub fn default_roots() -> Vec<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let mut roots = vec![home.join("Music/SuperDuper Samples"), home.join("Music/Favorite 808s")];
        if let Ok(rd) = std::fs::read_dir("/Volumes") {
            for v in rd.flatten() {
                let p = v.path().join("SuperDuper Samples");
                if p.is_dir() { roots.push(p); }
            }
        }
        roots.retain(|p| p.is_dir());
        roots
    }

    fn walk(dir: &Path, root: &Path, depth: usize, out: &mut Vec<(PathBuf, String)>) {
        if depth > 6 { return; }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name == "peaks" { continue; }
            if p.is_dir() { walk(&p, root, depth + 1, out); continue; }
            let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            if !matches!(ext.as_str(), "wav" | "flac" | "aif" | "aiff" | "ogg") { continue; }
            // The first folder under the root is the pack. A file lying in
            // the root itself takes the root's name. (This used to drop any
            // folder with a dot in its name — "Sounds of KSHMR Vol.3" became
            // "SuperDuper Samples".)
            let rel = p.strip_prefix(root).ok();
            let pack = rel
                .filter(|r| r.components().count() > 1)
                .and_then(|r| r.components().next())
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_else(|| root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
            out.push((p, pack));
        }
    }

    /// Scan `roots` for drum one-shots, classified by name. Each candidate
    /// is decoded once for a duration check (loops not named as loops get
    /// dropped); its features are cached in `~/.superduper-dsp/drum-library.json`
    /// keyed by size + mtime, so a rescan only decodes what changed.
    pub fn scan_library(roots: &[PathBuf], threads: usize) -> Vec<DrumSample> {
        let mut files = Vec::new();
        for r in roots { walk(r, r, 0, &mut files); }
        files.retain(|(p, _)| !looks_like_loop(p));
        files.sort();
        // The external drive mirrors some local packs; one copy per
        // (pack, file name) is enough, the first root wins.
        let mut seen = std::collections::HashSet::new();
        files.retain(|(p, pack)| seen.insert((pack.clone(), p.file_name().map(|f| f.to_os_string()))));

        let cache: Cache = std::fs::read(cache_path()).ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .filter(|c: &Cache| c.version == CACHE_VERSION)
            .unwrap_or_default();
        let cache = std::sync::Mutex::new(cache);
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results: Vec<std::sync::Mutex<Option<DrumSample>>> = files.iter().map(|_| std::sync::Mutex::new(None)).collect();
        std::thread::scope(|s| {
            for _ in 0..threads.max(1) {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some((path, pack)) = files.get(i) else { break };
                    let st = stamp(path);
                    let cached = cache.lock().ok().and_then(|c| c.entries.get(path).copied())
                        .filter(|(sz, mt, _)| Some((*sz, *mt)) == st)
                        .map(|(_, _, f)| f);
                    // Only files whose name says what they are. Sound-based
                    // classification hit the right pad 44-55% of the time
                    // (KSHMR / 909 / 808, examples/drum_classify.rs), and the
                    // folders it would cover are mostly not drums at all —
                    // vocals, synth shots, kubyz grains.
                    let Some(class) = classify_name(path) else { continue };
                    let feats = match cached {
                        Some(f) => Some(f),
                        None => decode_file(path).ok().map(|b| {
                            let f = audio_features(&mono_of(&b), b.sample_rate);
                            if let (Some((sz, mt)), Ok(mut c)) = (st, cache.lock()) { c.entries.insert(path.clone(), (sz, mt, f)); }
                            f
                        }),
                    };
                    let Some(f) = feats else { continue };
                    if f.duration_s > MAX_ONESHOT_S { continue; }
                    let source = ClassSource::Name;
                    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    if let Ok(mut slot) = results[i].lock() {
                        let named = key_from_name(path);
                        let heard = if is_tonal(class) { f.pitch_hz.map(pitch_class) } else { None };
                        *slot = Some(DrumSample {
                            path: path.clone(), pack: pack.clone(), name, class, source,
                            key: named.or(heard), key_guessed: named.is_none() && heard.is_some(),
                            duration_s: f.duration_s,
                        });
                    }
                });
            }
        });
        if let Ok(c) = cache.into_inner() {
            if let Some(dir) = cache_path().parent() { let _ = std::fs::create_dir_all(dir); }
            if let Ok(json) = serde_json::to_vec(&c) { let _ = std::fs::write(cache_path(), json); }
        }
        let mut out: Vec<DrumSample> = results.into_iter().filter_map(|m| m.into_inner().ok().flatten()).collect();
        out.sort_by(|a, b| (a.class, &a.pack, &a.name).cmp(&(b.class, &b.pack, &b.name)));
        out
    }

    /// The entries one pad browses, in a stable order.
    pub fn for_pad(lib: &[DrumSample], pad: usize) -> Vec<&DrumSample> {
        lib.iter().filter(|s| s.class.pad() == pad).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(p: &str) -> Option<DrumClass> { classify_name(Path::new(p)) }

    #[test]
    fn names_from_our_packs() {
        assert_eq!(c("KSHMR_Kicks/KSHMR_Big_Kick_01_F.wav"), Some(DrumClass::Kick));
        assert_eq!(c("KSHMR_Cymbals/KSHMR_Closed_Hat_03.wav"), Some(DrumClass::HatClosed));
        assert_eq!(c("KSHMR_Cymbals/KSHMR_Open_Hat_02.wav"), Some(DrumClass::HatOpen));
        assert_eq!(c("KSHMR_Cymbals/KSHMR_Ride_17.wav"), Some(DrumClass::Cymbal));
        assert_eq!(c("KSHMR_Claps/KSHMR_Clap_10.wav"), Some(DrumClass::Clap));
        assert_eq!(c("KSHMR_Percussion/KSHMR_Percussion_Flam_01_A.wav"), Some(DrumClass::Perc));
        assert_eq!(c("TR909all/909-BT0A0A7.WAV"), Some(DrumClass::Kick));
        assert_eq!(c("TR909all/909-ST0T0S3.WAV"), Some(DrumClass::Snare));
        assert_eq!(c("TR909all/909-HHCD0.WAV"), Some(DrumClass::HatClosed));
        assert_eq!(c("TR909all/909-HHOD0.WAV"), Some(DrumClass::HatOpen));
        assert_eq!(c("TR909all/909-HT0D0.WAV"), Some(DrumClass::Tom));
        assert_eq!(c("TR909all/909-RIDED0.WAV"), Some(DrumClass::Cymbal));
        assert_eq!(c("TR909all/909-HANDCLP1.WAV"), Some(DrumClass::Clap));
        assert_eq!(c("Cassette808_Samples/Cassette808_BD01.wav"), Some(DrumClass::Kick));
        assert_eq!(c("Cassette808_Samples/Cassette808_CP_01.wav"), Some(DrumClass::Clap));
        assert_eq!(c("Cassette808_Samples/Cassette808_conga01.wav"), Some(DrumClass::Perc));
        assert_eq!(c("Favorite 808s/808 (LifeIsGood).wav"), Some(DrumClass::Kick));
        assert_eq!(c("RUST 808/RUST808 Sub Cs1.wav"), Some(DrumClass::Kick));
        assert_eq!(c("kubyz-drumkit/kubyz_hat_open.wav"), Some(DrumClass::HatOpen));
        assert_eq!(c("kubyz-drumkit/kubyz_rim.wav"), Some(DrumClass::Perc));
        assert_eq!(c("misc/vocal_chop_07.wav"), None);
    }

    #[test]
    fn keys_from_names() {
        let k = |p: &str| key_from_name(Path::new(p));
        assert_eq!(k("KSHMR_Big_Kick_02_D.wav"), Some(2));
        assert_eq!(k("KSHMR_Big_Kick_03_D#.wav"), Some(3));
        assert_eq!(k("RUST808 Sub Cs1.wav"), Some(1));
        assert_eq!(k("808_Gm.wav"), Some(7));
        assert_eq!(k("KSHMR_Clap_10.wav"), None);
        assert_eq!(k("kubyz_kick.wav"), None);
    }

    #[test]
    fn loops_are_not_one_shots() {
        assert!(looks_like_loop(Path::new("KSHMR_Drums_Loops/KSHMR_Drum_Loop_01_128.wav")));
        assert!(looks_like_loop(Path::new("KSHMR_Fills/KSHMR_Tom_Fills/x.wav")));
        assert!(!looks_like_loop(Path::new("KSHMR_Kicks/KSHMR_Big_Kick_01_F.wav")));
    }

    #[test]
    fn sample_hit_plays_pitches_and_ends() {
        use std::sync::Arc;
        let buf = Arc::new(crate::sfz::SampleBuf { frames: 1000, channels: 1, sample_rate: 48_000.0, data: vec![16384i16; 1000].into_boxed_slice() });
        let mut h = SampleHit::default();
        h.start(buf.clone(), 48_000.0, 12.0, 10.0, 1.0);
        let mut n = 0;
        while h.is_active() && n < 5000 { let _ = h.process(); n += 1; }
        assert_eq!(n, 500, "an octave up plays 1000 frames in 500 steps");
        h.start(buf.clone(), 48_000.0, 0.0, 10.0, 1.0);
        let (l, _) = h.process();
        assert!((l - 0.5).abs() < 1e-3);
        h.choke(48_000.0);
        for _ in 0..1000 { let _ = h.process(); }
        assert!(!h.is_active(), "choke ends the hit");
        assert_eq!(Arc::strong_count(&buf), 1, "the hit released its clone");
    }

    #[test]
    fn drum_pitch_reads_the_tail_not_the_sweep() {
        // A kick: 3× sweep down to 55 Hz in 50 ms, then a 55 Hz body.
        let sr = 48_000.0;
        let mut ph = 0f32;
        let kick: Vec<f32> = (0..48_000).map(|i| {
            let t = i as f32 / sr;
            let f = 55.0 * (1.0 + 2.0 * (-t / 0.015).exp());
            ph += f / sr;
            (2.0 * std::f32::consts::PI * ph).sin() * (-t * 3.0).exp()
        }).collect();
        let hz = drum_pitch_hz(&kick, sr).expect("pitch");
        assert!((hz - 55.0).abs() < 1.5, "{hz}");
        assert_eq!(pitch_class(hz), 9, "55 Hz is A");
        assert_eq!(key_shift(9, 2), 5);   // A → D: up a fourth
        assert_eq!(key_shift(9, 4), -5);  // A → E: down a fourth, not up 7
        assert_eq!(key_shift(0, 6), -6);
        assert_eq!(parse_key_name("F#m"), Some(6));
        assert_eq!(parse_key_name("Bb major"), Some(10));
        assert_eq!(parse_key_name(" a minor\n"), Some(9));
        assert_eq!(parse_key_name("x"), None);
    }

    #[test]
    fn audio_rules_on_synthetic_hits() {
        let sr = 48_000.0;
        let n = 24_000;
        let mut rng = crate::dsp_blocks::Xorshift::new(7);
        let kick: Vec<f32> = (0..n).map(|i| { let t = i as f32 / sr; (2.0 * std::f32::consts::PI * 55.0 * t).sin() * (-t * 8.0).exp() }).collect();
        let hat: Vec<f32> = (0..n).map(|i| {
            let t = i as f32 / sr;
            let x = rng.next_bipolar();
            x * (-t * 40.0).exp()
        }).collect();
        // White noise is flat, but its centroid is ~sr/4 — exactly what a hat
        // looks like to these features.
        let fk = audio_features(&kick, sr);
        let fh = audio_features(&hat, sr);
        assert_eq!(classify_audio(&fk), DrumClass::Kick, "{fk:?}");
        assert_eq!(classify_audio(&fh), DrumClass::HatClosed, "{fh:?}");
    }
}
