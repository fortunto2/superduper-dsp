//! sdsp-transcribe: audio to MIDI.
//!
//!   sdsp-transcribe in.wav [--mode any|guitar] [--capo N] [--maxfret N]
//!                          [--onset X] [--frame X] --out x.mid [--json x.json] [--bpm 120]
//!
//! Any pitched instrument by default (Basic Pitch's own note creation); `--mode guitar` names
//! the string and fret and writes the string as the MIDI channel. `--raw dir` also dumps the
//! posteriors (note.f32, onset.f32; frames x 88, little endian) for parity checks.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use superduper_transcribe::{midi, resample, Mode, Settings, Transcriber};

fn read_wav(path: &str) -> Result<(Vec<f32>, u32)> {
    let mut r = hound::WavReader::open(path).with_context(|| format!("open {path}"))?;
    let spec = r.spec();
    let ch = spec.channels as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>()?
        }
    };
    let planar: Vec<Vec<f32>> = (0..ch).map(|c| interleaved.iter().skip(c).step_by(ch).copied().collect()).collect();
    let refs: Vec<&[f32]> = planar.iter().map(|v| v.as_slice()).collect();
    Ok((resample::mixdown(&refs), spec.sample_rate))
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mut input = None;
    let (mut out, mut json, mut raw): (Option<PathBuf>, Option<PathBuf>, Option<PathBuf>) = (None, None, None);
    let mut s = Settings::default();
    let mut bpm = 120.0;
    while let Some(a) = args.next() {
        let mut val = |name: &str| args.next().with_context(|| format!("{name} needs a value"));
        match a.as_str() {
            "--mode" => {
                s.mode = match val("--mode")?.as_str() {
                    "any" => Mode::Any,
                    "guitar" => Mode::Guitar,
                    m => bail!("--mode any|guitar, got {m}"),
                }
            }
            "--capo" => s.capo = val("--capo")?.parse()?,
            "--maxfret" => s.max_fret = val("--maxfret")?.parse()?,
            "--onset" => s.onset_thr = val("--onset")?.parse()?,
            "--frame" => s.frame_thr = val("--frame")?.parse()?,
            "--bpm" => bpm = val("--bpm")?.parse()?,
            "--out" => out = Some(val("--out")?.into()),
            "--json" => json = Some(val("--json")?.into()),
            "--raw" => raw = Some(val("--raw")?.into()),
            "-h" | "--help" => {
                println!("sdsp-transcribe in.wav [--mode any|guitar] [--capo N] [--maxfret N] [--onset X] [--frame X] [--bpm N] --out x.mid [--json x.json]");
                return Ok(());
            }
            _ if input.is_none() && !a.starts_with("--") => input = Some(a),
            _ => bail!("unknown argument {a}"),
        }
    }
    let input = input.context("input wav missing (see --help)")?;
    let (mono, sr) = read_wav(&input)?;
    let t0 = std::time::Instant::now();
    let tr = Transcriber::new().map_err(anyhow::Error::msg)?;
    let post = tr.posteriors(&mono, sr, |_| true).map_err(anyhow::Error::msg)?.context("cancelled")?;
    let notes = superduper_transcribe::decode(&post, &s);
    eprintln!(
        "{input}: {:.1} s audio, {} frames, {} notes in {:.2} s",
        mono.len() as f64 / sr as f64,
        post.frames,
        notes.len(),
        t0.elapsed().as_secs_f64()
    );
    if let Some(dir) = raw {
        std::fs::create_dir_all(&dir)?;
        let bytes = |v: &[f32]| v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
        std::fs::write(dir.join("note.f32"), bytes(&post.note))?;
        std::fs::write(dir.join("onset.f32"), bytes(&post.onset))?;
    }
    if let Some(p) = out {
        std::fs::write(&p, midi::write_smf(&notes, bpm))?;
        eprintln!("wrote {}", p.display());
    }
    if let Some(p) = json {
        std::fs::write(&p, serde_json::to_string_pretty(&notes)?)?;
        eprintln!("wrote {}", p.display());
    }
    Ok(())
}
