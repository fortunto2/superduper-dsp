//! Minimal RIFF/WAV reader — enough to load drum-machine / sampler
//! one-shots without pulling in `hound` and its full read/write API.
//!
//! Supported formats:
//! - PCM (format tag 1): 16-bit, 24-bit, 32-bit signed integer
//! - IEEE float (format tag 3): 32-bit float
//! - WAVE_FORMAT_EXTENSIBLE (format tag 0xFFFE): same encodings via
//!   the embedded sub-format GUID
//!
//! Mono and stereo only. The decoded samples are returned interleaved
//! when stereo (L, R, L, R, …) so the caller decides whether to
//! treat them as two channels or sum to mono.

#[derive(Debug)]
pub enum WavError {
    Io(std::io::Error),
    NotRiff,
    NotWave,
    MissingFmt,
    MissingData,
    UnsupportedFormat { tag: u16, bits_per_sample: u16 },
    UnsupportedChannelCount(u16),
    TruncatedChunk,
}

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WavError::Io(e) => write!(f, "I/O error: {}", e),
            WavError::NotRiff => write!(f, "not a RIFF file"),
            WavError::NotWave => write!(f, "not a WAVE file"),
            WavError::MissingFmt => write!(f, "missing fmt chunk"),
            WavError::MissingData => write!(f, "missing data chunk"),
            WavError::UnsupportedFormat { tag, bits_per_sample } => {
                write!(f, "unsupported WAV format tag={} bits={}", tag, bits_per_sample)
            }
            WavError::UnsupportedChannelCount(c) => write!(f, "unsupported channel count: {}", c),
            WavError::TruncatedChunk => write!(f, "truncated chunk"),
        }
    }
}

impl std::error::Error for WavError {}

/// Decoded WAV payload. `samples` is interleaved for multi-channel
/// (L, R, L, R, …) so the index can be computed by frame × channels.
pub struct WavData {
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl WavData {
    /// Number of audio frames (samples per channel).
    pub fn frame_count(&self) -> usize {
        if self.channels == 0 { 0 }
        else { self.samples.len() / self.channels as usize }
    }
    /// Read a single mono frame (sums channels if stereo).
    #[inline]
    pub fn read_mono_at(&self, frame: usize) -> f32 {
        let ch = self.channels as usize;
        let base = frame * ch;
        if base + ch > self.samples.len() { return 0.0; }
        let mut sum = 0.0_f32;
        for c in 0..ch { sum += self.samples[base + c]; }
        sum / ch as f32
    }
    /// Read a stereo pair at the given frame; duplicates the L into R
    /// when the source is mono.
    #[inline]
    pub fn read_stereo_at(&self, frame: usize) -> (f32, f32) {
        let ch = self.channels as usize;
        let base = frame * ch;
        if base + ch > self.samples.len() { return (0.0, 0.0); }
        if ch == 1 { (self.samples[base], self.samples[base]) }
        else { (self.samples[base], self.samples[base + 1]) }
    }
}

pub fn parse_wav_file(path: &std::path::Path) -> Result<WavData, WavError> {
    let bytes = std::fs::read(path).map_err(WavError::Io)?;
    parse_wav_bytes(&bytes)
}

pub fn parse_wav_bytes(b: &[u8]) -> Result<WavData, WavError> {
    if b.len() < 12 { return Err(WavError::TruncatedChunk); }
    if &b[0..4] != b"RIFF" { return Err(WavError::NotRiff); }
    if &b[8..12] != b"WAVE" { return Err(WavError::NotWave); }

    let mut sample_rate = 0u32;
    let mut channels = 0u16;
    let mut bits_per_sample = 0u16;
    let mut format_tag = 0u16;
    let mut samples = Vec::new();
    let mut fmt_seen = false;
    let mut data_seen = false;

    let mut i = 12usize;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let size = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]) as usize;
        i += 8;
        let end = i + size;
        if end > b.len() { return Err(WavError::TruncatedChunk); }
        let chunk = &b[i..end];
        match id {
            b"fmt " => {
                if chunk.len() < 16 { return Err(WavError::TruncatedChunk); }
                format_tag = u16::from_le_bytes([chunk[0], chunk[1]]);
                channels = u16::from_le_bytes([chunk[2], chunk[3]]);
                sample_rate = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
                bits_per_sample = u16::from_le_bytes([chunk[14], chunk[15]]);
                // WAVE_FORMAT_EXTENSIBLE — peek at the sub-format GUID's
                // first 2 bytes which match the legacy format tag.
                if format_tag == 0xFFFE && chunk.len() >= 26 {
                    let sub_tag = u16::from_le_bytes([chunk[24], chunk[25]]);
                    format_tag = sub_tag;
                }
                fmt_seen = true;
            }
            b"data" => {
                if !fmt_seen { return Err(WavError::MissingFmt); }
                samples = decode_samples(chunk, format_tag, bits_per_sample)?;
                data_seen = true;
            }
            _ => {}
        }
        // Chunks are padded to even byte count.
        i = end + (size & 1);
    }
    if !fmt_seen { return Err(WavError::MissingFmt); }
    if !data_seen { return Err(WavError::MissingData); }
    if channels == 0 || channels > 2 {
        return Err(WavError::UnsupportedChannelCount(channels));
    }
    Ok(WavData { sample_rate, channels, samples })
}

fn decode_samples(b: &[u8], fmt: u16, bps: u16) -> Result<Vec<f32>, WavError> {
    match (fmt, bps) {
        (1, 16) => Ok(b
            .chunks_exact(2)
            .map(|c| (i16::from_le_bytes([c[0], c[1]]) as f32) / 32_768.0)
            .collect()),
        (1, 24) => Ok(b
            .chunks_exact(3)
            .map(|c| {
                let v = (c[0] as i32) | ((c[1] as i32) << 8) | ((c[2] as i32) << 16);
                // Sign-extend the 24-bit signed value into i32.
                let signed = if v & 0x0080_0000 != 0 { v | (-1 ^ 0x00FF_FFFF) } else { v };
                (signed as f32) / 8_388_608.0
            })
            .collect()),
        (1, 32) => Ok(b
            .chunks_exact(4)
            .map(|c| {
                let v = i32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                (v as f32) / 2_147_483_648.0
            })
            .collect()),
        (3, 32) => Ok(b
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()),
        _ => Err(WavError::UnsupportedFormat { tag: fmt, bits_per_sample: bps }),
    }
}

// ---------------------------------------------------------------------------
// Mono 32-bit float WAV writer.
//
// Used for wavetable export — Serum, Vital, Phase Plant and most other
// wavetable synths read single-cycle waveforms from this exact format
// (RIFF + fmt tag 3 + 32-bit float + mono). Sample rate is mostly a
// label for single-cycle data; we use 88200 Hz to match Serum's
// convention (any rate works, but 88200 makes a 2048-sample table
// equivalent to ~43 Hz which is below the audible range and helps
// pitch-shifting tools).
// ---------------------------------------------------------------------------

pub const SINGLE_CYCLE_SAMPLE_RATE: u32 = 88200;

/// Write `samples` to `path` as a mono 32-bit float WAV. Sample rate
/// embedded in the header is `sample_rate` (use 88200 for single-cycle
/// wavetables to match Serum / Vital convention). Existing file is
/// overwritten.
pub fn write_mono_f32_wav(
    path: &std::path::Path,
    samples: &[f32],
    sample_rate: u32,
) -> Result<(), WavError> {
    let mut out = Vec::with_capacity(44 + samples.len() * 4);
    // RIFF chunk
    out.extend_from_slice(b"RIFF");
    let chunk_size = (36 + samples.len() * 4) as u32;
    out.extend_from_slice(&chunk_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    // fmt sub-chunk — format tag 3 = IEEE float.
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());      // sub-chunk size
    out.extend_from_slice(&3u16.to_le_bytes());       // format = float
    out.extend_from_slice(&1u16.to_le_bytes());       // channels = 1
    out.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * 4;                  // mono float32 → 4 bytes/sample
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());       // block align
    out.extend_from_slice(&32u16.to_le_bytes());      // bits per sample
    // data sub-chunk
    out.extend_from_slice(b"data");
    let data_size = (samples.len() * 4) as u32;
    out.extend_from_slice(&data_size.to_le_bytes());
    for &s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, out).map_err(WavError::Io)
}

/// Read `path` as a "Serum-style stitched" wavetable: if the file's
/// mono-fold length is an exact multiple of `frame_len` AND the
/// quotient is in [1, max_frames], split into that many cycles
/// (each one `frame_len` samples, no resampling). Otherwise return
/// `None` — caller should fall back to single-cycle extraction
/// (pitch detect + period extract or full-file resample).
///
/// Stereo input averages L+R into mono before the length check.
pub fn read_stitched_wavetable(
    path: &std::path::Path,
    frame_len: usize,
    max_frames: usize,
) -> Result<Option<Vec<Vec<f32>>>, WavError> {
    let wav = parse_wav_file(path)?;
    let frames = wav.frame_count();
    if frames == 0 || frame_len == 0 {
        return Ok(None);
    }
    if frames % frame_len != 0 {
        return Ok(None);
    }
    let n = frames / frame_len;
    if n == 0 || n > max_frames {
        return Ok(None);
    }
    // Mono-fold while splitting into per-frame chunks.
    let mut out = Vec::with_capacity(n);
    for k in 0..n {
        let mut chunk = Vec::with_capacity(frame_len);
        for i in 0..frame_len {
            chunk.push(wav.read_mono_at(k * frame_len + i));
        }
        out.push(chunk);
    }
    Ok(Some(out))
}

/// Write N single-cycle frames as one Serum-style stitched WAV —
/// mono 32-bit float, samples are `frames[0]…frames[N-1]`
/// concatenated end-to-end. Files written this way are accepted as
/// wavetables by Serum / Vital / Phase Plant / Pigments / any synth
/// that follows the convention.
pub fn write_stitched_wavetable(
    path: &std::path::Path,
    frames: &[Vec<f32>],
    sample_rate: u32,
) -> Result<(), WavError> {
    let total: usize = frames.iter().map(|f| f.len()).sum();
    let mut flat = Vec::with_capacity(total);
    for f in frames {
        flat.extend_from_slice(f);
    }
    write_mono_f32_wav(path, &flat, sample_rate)
}

/// Read `path` as a single-cycle mono WAV and resample / truncate /
/// pad to exactly `target_len` samples. Handles any sample-rate the
/// file claims (we ignore it) and either stereo (averaged to mono)
/// or mono input. Returns the resampled curve in `[-1, +1]` range.
///
/// Resampling is **linear interpolation** — for a single-cycle
/// waveform that's already 2k+ samples this is inaudible; the proper
/// bandlimit is handled later by the mip pyramid.
pub fn read_single_cycle_wav(
    path: &std::path::Path,
    target_len: usize,
) -> Result<Vec<f32>, WavError> {
    let wav = parse_wav_file(path)?;
    let frames = wav.frame_count();
    if frames == 0 {
        return Err(WavError::MissingData);
    }
    let mut out = Vec::with_capacity(target_len);
    if frames == target_len {
        for i in 0..target_len {
            out.push(wav.read_mono_at(i));
        }
        return Ok(out);
    }
    // Resample: read at position t * (frames-1) / (target_len-1).
    let denom = (target_len.max(2) - 1) as f32;
    for i in 0..target_len {
        let src = (i as f32) * (frames as f32) / denom;
        let i0 = src.floor() as usize;
        let frac = src - i0 as f32;
        let i0 = i0.min(frames - 1);
        let i1 = (i0 + 1).min(frames - 1);
        let s0 = wav.read_mono_at(i0);
        let s1 = wav.read_mono_at(i1);
        out.push(s0 + (s1 - s0) * frac);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal 16-bit mono PCM WAV from a sample slice, parse
    /// it back, assert round-trip is sample-accurate (within i16
    /// quantisation).
    fn build_wav_16(samples: &[f32], sr: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let data_size = (samples.len() * 2) as u32;
        let riff_size = 36 + data_size;
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&riff_size.to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&1u16.to_le_bytes()); // mono
        out.extend_from_slice(&sr.to_le_bytes());
        out.extend_from_slice(&(sr * 2).to_le_bytes()); // byte rate
        out.extend_from_slice(&2u16.to_le_bytes()); // block align
        out.extend_from_slice(&16u16.to_le_bytes()); // bps
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_size.to_le_bytes());
        for &s in samples {
            let q = (s.clamp(-1.0, 1.0) * 32_767.0) as i16;
            out.extend_from_slice(&q.to_le_bytes());
        }
        out
    }

    #[test]
    fn roundtrip_16bit_mono() {
        let input: Vec<f32> = (0..256)
            .map(|i| (i as f32 / 256.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        let bytes = build_wav_16(&input, 48000);
        let parsed = parse_wav_bytes(&bytes).expect("parse");
        assert_eq!(parsed.sample_rate, 48000);
        assert_eq!(parsed.channels, 1);
        assert_eq!(parsed.samples.len(), input.len());
        // Allow ~2 LSB error — round-trip uses 32767 for encode and
        // 32768 for decode (intentional, keeps -1.0 reachable).
        for (a, b) in input.iter().zip(parsed.samples.iter()) {
            assert!((a - b).abs() < 1.0 / 16_000.0, "delta {}", a - b);
        }
    }

    #[test]
    fn write_then_read_f32_mono() {
        let input: Vec<f32> = (0..2048)
            .map(|i| (i as f32 / 2048.0 * std::f32::consts::TAU).sin())
            .collect();
        let tmp = std::env::temp_dir().join(format!(
            "sdsp-wav-test-{}.wav",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_mono_f32_wav(&tmp, &input, SINGLE_CYCLE_SAMPLE_RATE).expect("write");
        let out = read_single_cycle_wav(&tmp, 2048).expect("read");
        assert_eq!(out.len(), 2048);
        for (a, b) in input.iter().zip(out.iter()) {
            assert!((a - b).abs() < 1e-6, "delta {}", (a - b));
        }
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn stitched_wavetable_round_trip() {
        // 4 different frames, each 2048 samples, concatenated and
        // written as one WAV. Reading back must reconstruct identical
        // per-frame contents.
        let frames: Vec<Vec<f32>> = (0..4)
            .map(|k| {
                (0..2048)
                    .map(|i| {
                        let phase = i as f32 / 2048.0 * std::f32::consts::TAU;
                        (phase * (k as f32 + 1.0)).sin()
                    })
                    .collect()
            })
            .collect();
        let tmp = std::env::temp_dir().join(format!(
            "sdsp-stitched-{}.wav",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_stitched_wavetable(&tmp, &frames, SINGLE_CYCLE_SAMPLE_RATE).unwrap();
        let parsed = read_stitched_wavetable(&tmp, 2048, 16)
            .unwrap()
            .expect("4 × 2048 should split");
        assert_eq!(parsed.len(), 4);
        for (orig, decoded) in frames.iter().zip(parsed.iter()) {
            assert_eq!(orig.len(), decoded.len());
            for (a, b) in orig.iter().zip(decoded.iter()) {
                assert!((a - b).abs() < 1e-6, "stitched delta {}", (a - b).abs());
            }
        }
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn stitched_returns_none_for_non_multiple_length() {
        // 1500-sample sine — not a multiple of 2048, so the splitter
        // refuses (caller falls back to pitch-detect single-cycle).
        let src: Vec<f32> = (0..1500)
            .map(|i| (i as f32 / 1500.0 * std::f32::consts::TAU).sin())
            .collect();
        let tmp = std::env::temp_dir().join(format!(
            "sdsp-stitched-bad-{}.wav",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_mono_f32_wav(&tmp, &src, 44100).unwrap();
        let parsed = read_stitched_wavetable(&tmp, 2048, 16).unwrap();
        assert!(parsed.is_none());
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn read_single_cycle_resamples_to_target_len() {
        // 512-sample input → 2048-sample output (4× upsample via linear interp).
        let input: Vec<f32> = (0..512)
            .map(|i| (i as f32 / 512.0 * std::f32::consts::TAU).sin())
            .collect();
        let tmp = std::env::temp_dir().join(format!(
            "sdsp-wav-resample-{}.wav",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        write_mono_f32_wav(&tmp, &input, 44100).expect("write");
        let out = read_single_cycle_wav(&tmp, 2048).expect("read");
        assert_eq!(out.len(), 2048);
        // Should still roughly be a sine wave — peak at ~quarter cycle.
        let peak_idx = out
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert!(
            (peak_idx as i32 - 2048 / 4).abs() < 8,
            "peak at {peak_idx}, expected near {}",
            2048 / 4
        );
        std::fs::remove_file(&tmp).ok();
    }
}
