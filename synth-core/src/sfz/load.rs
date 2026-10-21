//! Filesystem half: read an `.sfz` with its includes, decode every sample in
//! parallel, and find packs on disk. Not RT-safe — main/worker thread only.
//!
//! Decoded samples live in a process-wide cache keyed by absolute path, so
//! two Piano instances on two tracks share one copy of the 1.3 GB Salamander
//! instead of holding two.

use super::engine::{Instrument, SampleBuf};
use super::parse::{parse_sfz, ParseError};
use super::region::compile;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read {0}: {1}")]
    Read(PathBuf, std::io::Error),
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error("{0} has no playable regions")]
    Empty(PathBuf),
}

/// Progress of a load, readable from another thread (GUI).
#[derive(Debug, Default)]
pub struct LoadProgress {
    pub done: AtomicUsize,
    pub total: AtomicUsize,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Weak<SampleBuf>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Weak<SampleBuf>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve a path, falling back to a case-insensitive match per component —
/// libraries authored on Windows routinely say `Samples/c4.WAV` for `C4.wav`.
pub fn resolve_case_insensitive(base: &Path, rel: &str) -> Option<PathBuf> {
    let direct = base.join(rel);
    if direct.exists() { return Some(direct); }
    let mut cur = base.to_path_buf();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        if part == ".." { cur.pop(); continue; }
        if part == "." { continue; }
        let exact = cur.join(part);
        if exact.exists() { cur = exact; continue; }
        let lower = part.to_lowercase();
        let found = std::fs::read_dir(&cur).ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().to_lowercase() == lower)?;
        cur = found.path();
    }
    Some(cur)
}

/// Decode an audio file to interleaved i16 at its native rate. More than two
/// channels keeps the first two.
pub fn decode_file(path: &Path) -> Result<SampleBuf, String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) { hint.with_extension(ext); }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| e.to_string())?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("no audio track")?;
    let track_id = track.id;
    let sr = track.codec_params.sample_rate.ok_or("unknown sample rate")? as f32;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| e.to_string())?;
    let mut channels = 0usize;
    let mut out: Vec<i16> = Vec::new();
    if let Some(n) = track.codec_params.n_frames { out.reserve(n as usize * 2); }
    let mut buf: Option<SampleBuffer<i16>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id() != track_id { continue; }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        };
        let spec = *decoded.spec();
        let ch = spec.channels.count();
        if buf.as_ref().is_none_or(|b| b.capacity() < decoded.capacity() * ch) {
            buf = Some(SampleBuffer::<i16>::new(decoded.capacity() as u64, spec));
        }
        let Some(b) = buf.as_mut() else { continue };
        b.copy_interleaved_ref(decoded);
        channels = ch;
        if ch <= 2 {
            out.extend_from_slice(b.samples());
        } else {
            for frame in b.samples().chunks(ch) { out.extend_from_slice(&frame[..2]); }
        }
    }
    let channels = channels.clamp(1, 2);
    let frames = out.len() / channels;
    out.truncate(frames * channels);
    Ok(SampleBuf { frames, channels, sample_rate: sr, data: out.into_boxed_slice() })
}

fn load_cached(path: &Path) -> Result<Arc<SampleBuf>, String> {
    if let Ok(c) = cache().lock() {
        if let Some(s) = c.get(path).and_then(Weak::upgrade) { return Ok(s); }
    }
    let s = Arc::new(decode_file(path)?);
    if let Ok(mut c) = cache().lock() {
        c.retain(|_, w| w.strong_count() > 0);
        c.insert(path.to_path_buf(), Arc::downgrade(&s));
    }
    Ok(s)
}

type Decoded = Result<Arc<SampleBuf>, String>;

/// Parse + decode. `threads` workers decode in parallel; `progress` (if
/// given) counts finished samples.
pub fn load_instrument(sfz: &Path, threads: usize, progress: Option<&LoadProgress>) -> Result<Instrument, LoadError> {
    let text = std::fs::read_to_string(sfz).map_err(|e| LoadError::Read(sfz.to_path_buf(), e))?;
    let dir = sfz.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut read = |p: &str| {
        resolve_case_insensitive(&dir, &p.replace('\\', "/")).and_then(|f| std::fs::read_to_string(f).ok())
    };
    let def = compile(&parse_sfz(&text, &mut read)?);
    if def.regions.is_empty() { return Err(LoadError::Empty(sfz.to_path_buf())); }

    let paths: Vec<Option<PathBuf>> = def.sample_paths.iter().map(|p| resolve_case_insensitive(&dir, p)).collect();
    if let Some(pr) = progress {
        pr.total.store(paths.len(), Ordering::Relaxed);
        pr.done.store(0, Ordering::Relaxed);
    }
    let slots: Vec<Mutex<Option<Decoded>>> = paths.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads.max(1) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(p) = paths.get(i) else { break };
                let res = match p {
                    Some(p) => load_cached(p),
                    None => Err("file not found".into()),
                };
                if let Ok(mut slot) = slots[i].lock() { *slot = Some(res); }
                if let Some(pr) = progress { pr.done.fetch_add(1, Ordering::Relaxed); }
            });
        }
    });

    let mut samples = Vec::with_capacity(slots.len());
    let mut missing = Vec::new();
    for (i, slot) in slots.into_iter().enumerate() {
        match slot.into_inner().ok().flatten() {
            Some(Ok(s)) => samples.push(s),
            other => {
                let why = match other { Some(Err(e)) => e, _ => "not decoded".into() };
                missing.push(format!("{}: {why}", def.sample_paths[i]));
                samples.push(Arc::new(SampleBuf::silent()));
            }
        }
    }
    let name = sfz.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Instrument { def, samples, missing, name })
}

/// One playable `.sfz` found on disk.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PackEntry {
    /// First folder under the root — what a user dropped in.
    pub pack: String,
    /// The `.sfz` file stem.
    pub name: String,
    pub path: PathBuf,
}

/// Every `.sfz` under `root` (depth ≤ 4), sorted by (pack, name) so an index
/// is stable as long as the folder content is.
pub fn scan_packs(root: &Path) -> Vec<PackEntry> {
    fn walk(dir: &Path, pack: Option<&str>, depth: usize, out: &mut Vec<PackEntry>) {
        if depth > 4 { return; }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let fname = e.file_name().to_string_lossy().into_owned();
            if fname.starts_with('.') { continue; }
            if p.is_dir() {
                walk(&p, Some(pack.unwrap_or(&fname)), depth + 1, out);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("sfz")) {
                let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                out.push(PackEntry { pack: pack.unwrap_or("Loose").to_string(), name, path: p });
            }
        }
    }
    let mut out = Vec::new();
    walk(root, None, 0, &mut out);
    out.sort();
    out
}

/// `~/Music/SFZ` — where packs go.
pub fn default_pack_root() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join("Music").join("SFZ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sfz-load-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("tmp");
        d
    }

    fn write_wav(path: &Path, frames: usize, value: i16) {
        let spec = hound::WavSpec { channels: 1, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(path, spec).expect("wav");
        for _ in 0..frames { w.write_sample(value).expect("write"); }
        w.finalize().expect("finalize");
    }

    #[test]
    fn loads_wav_with_include_and_case_mismatch() {
        let d = tmpdir("a");
        std::fs::create_dir_all(d.join("Samples")).expect("mkdir");
        write_wav(&d.join("Samples").join("C4.wav"), 1000, 1234);
        std::fs::write(d.join("regions.txt"), "<region> sample=c4.WAV key=60").expect("write");
        std::fs::write(d.join("Test.sfz"), "<control> default_path=samples/\n#include \"regions.txt\"").expect("write");
        let inst = load_instrument(&d.join("Test.sfz"), 2, None).expect("load");
        assert!(inst.missing.is_empty(), "{:?}", inst.missing);
        assert_eq!(inst.samples[0].frames, 1000);
        assert_eq!(inst.samples[0].sample_rate, 44_100.0);
        assert_eq!(inst.samples[0].data[10], 1234);
    }

    #[test]
    fn missing_sample_is_reported_and_silent() {
        let d = tmpdir("b");
        std::fs::write(d.join("T.sfz"), "<region> sample=nope.wav").expect("write");
        let inst = load_instrument(&d.join("T.sfz"), 1, None).expect("load");
        assert_eq!(inst.missing.len(), 1);
        assert_eq!(inst.samples[0].frames, 0);
    }

    #[test]
    fn scan_finds_packs_by_top_folder() {
        let d = tmpdir("c");
        std::fs::create_dir_all(d.join("Piano").join("sub")).expect("mkdir");
        std::fs::write(d.join("Piano").join("sub").join("Grand.sfz"), "").expect("w");
        std::fs::write(d.join("Piano").join("Soft.SFZ"), "").expect("w");
        std::fs::write(d.join("loose.sfz"), "").expect("w");
        let names: Vec<(String, String)> = scan_packs(&d).into_iter().map(|e| (e.pack, e.name)).collect();
        assert_eq!(names, vec![
            ("Loose".into(), "loose".into()),
            ("Piano".into(), "Grand".into()),
            ("Piano".into(), "Soft".into()),
        ]);
    }
}
