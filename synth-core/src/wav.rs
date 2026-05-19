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
}
