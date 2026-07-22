//! WAV I/O for the mashup renderer.
//!
//! Decoding rides on `synth_core::wav` (RIFF reader shared with the plugins).
//! synth-core only ships a *mono* float writer, so the interleaved-stereo
//! writer the mashup output needs lives here.

use std::path::Path;

use superduper_synth_core::wav::{parse_wav_file, WavError};

/// A decoded stereo buffer at its native sample rate.
pub struct StereoWav {
    pub sample_rate: u32,
    pub l: Vec<f32>,
    pub r: Vec<f32>,
}

impl StereoWav {
    pub fn frames(&self) -> usize {
        self.l.len().min(self.r.len())
    }
}

/// Decode a WAV file to deinterleaved stereo f32. Mono sources are
/// duplicated to both channels.
pub fn decode_stereo(path: &Path) -> Result<StereoWav, WavError> {
    let wav = parse_wav_file(path)?;
    let frames = wav.frame_count();
    let mut l = Vec::with_capacity(frames);
    let mut r = Vec::with_capacity(frames);
    for i in 0..frames {
        let (a, b) = wav.read_stereo_at(i);
        l.push(a);
        r.push(b);
    }
    Ok(StereoWav {
        sample_rate: wav.sample_rate,
        l,
        r,
    })
}

/// Decode any audio file to stereo f32. `.wav` uses the built-in RIFF reader;
/// everything else (FLAC / MP3 / …) is decoded via ffmpeg to 44.1 kHz stereo.
/// This lets a `[[track]]` reference a raw song or jingle, not only demucs WAVs.
pub fn decode_any(path: &Path) -> Result<StereoWav, String> {
    let is_wav = path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("wav"))
        .unwrap_or(false);
    if is_wav {
        return decode_stereo(path).map_err(|e| format!("{e}"));
    }
    let out = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(path)
        .args(["-ac", "2", "-ar", "44100", "-f", "f32le", "-"])
        .output()
        .map_err(|e| format!("ffmpeg spawn failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ffmpeg failed to decode {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let bytes = out.stdout;
    let frames = bytes.len() / 8; // 2ch × 4 bytes
    let mut l = Vec::with_capacity(frames);
    let mut r = Vec::with_capacity(frames);
    for f in 0..frames {
        let o = f * 8;
        l.push(f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]));
        r.push(f32::from_le_bytes([bytes[o + 4], bytes[o + 5], bytes[o + 6], bytes[o + 7]]));
    }
    Ok(StereoWav { sample_rate: 44_100, l, r })
}

/// Write deinterleaved stereo f32 as a 32-bit-float (format tag 3) WAV.
pub fn write_stereo_f32_wav(
    path: &Path,
    l: &[f32],
    r: &[f32],
    sample_rate: u32,
) -> Result<(), WavError> {
    let frames = l.len().min(r.len());
    let n_samples = frames * 2;
    let mut out = Vec::with_capacity(44 + n_samples * 4);
    // RIFF header
    out.extend_from_slice(b"RIFF");
    let chunk_size = (36 + n_samples * 4) as u32;
    out.extend_from_slice(&chunk_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    // fmt sub-chunk — format tag 3 = IEEE float, 2 channels.
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // sub-chunk size
    out.extend_from_slice(&3u16.to_le_bytes()); // format = float
    out.extend_from_slice(&2u16.to_le_bytes()); // channels = 2
    out.extend_from_slice(&sample_rate.to_le_bytes());
    let block_align = 2u16 * 4; // 2ch × 4 bytes
    let byte_rate = sample_rate * block_align as u32;
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes()); // bits per sample
    // data sub-chunk — interleaved L, R.
    out.extend_from_slice(b"data");
    let data_size = (n_samples * 4) as u32;
    out.extend_from_slice(&data_size.to_le_bytes());
    for i in 0..frames {
        out.extend_from_slice(&l[i].to_le_bytes());
        out.extend_from_slice(&r[i].to_le_bytes());
    }
    std::fs::write(path, out).map_err(WavError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_round_trip() {
        let sr = 48_000;
        let l: Vec<f32> = (0..1000).map(|n| (n as f32 / 1000.0) - 0.5).collect();
        let r: Vec<f32> = (0..1000).map(|n| 0.25 - (n as f32 / 2000.0)).collect();
        let dir = std::env::temp_dir();
        let path = dir.join(format!("sdsp_mash_wavio_{}.wav", std::process::id()));
        write_stereo_f32_wav(&path, &l, &r, sr).expect("write");
        let back = decode_stereo(&path).expect("decode");
        assert_eq!(back.sample_rate, sr);
        assert_eq!(back.frames(), 1000);
        for i in 0..1000 {
            assert!((back.l[i] - l[i]).abs() < 1e-6, "L[{i}]");
            assert!((back.r[i] - r[i]).abs() < 1e-6, "R[{i}]");
        }
        let _ = std::fs::remove_file(&path);
    }
}
