//! Audio engine for sdsp-runner.
//!
//! Pipeline:
//!   WAV (in memory) → CLAP plugin process → cpal output stream → speakers.
//!
//! cpal output callback runs on the audio thread and pulls from a global
//! state: WAV position, plugin processor, scratch buffers. We don't sweat
//! lock-free concurrency for v1 — the only writer of plugin state is the
//! audio thread, the main thread just polls "is the stream over yet".

use anyhow::{Context, Result};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::host::{host_info, Host, HostShared, LoadedPlugin};

pub struct WavContent {
    pub samples_l: Vec<f32>,
    pub samples_r: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
}

impl WavContent {
    pub fn frames(&self) -> usize { self.samples_l.len() }

    /// Build a zero-filled "wav" — useful when the user runs the runner
    /// without supplying any input (e.g. testing a synth plugin once we
    /// add MIDI support).
    pub fn silence(sample_rate: u32, channels: u16, frames: usize) -> Self {
        Self {
            samples_l: vec![0.0; frames],
            samples_r: vec![0.0; frames],
            channels,
            sample_rate,
        }
    }
}

pub fn load_wav(path: &Path) -> Result<WavContent> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let channels = spec.channels;
    let sample_rate = spec.sample_rate;

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .context("reading f32 samples")?,
        hound::SampleFormat::Int => {
            let max = ((1_i64 << (spec.bits_per_sample - 1)) - 1) as f32;
            reader
                .samples::<i32>()
                .map(|r| r.map(|v| v as f32 / max))
                .collect::<Result<Vec<_>, _>>()
                .context("reading int samples")?
        }
    };

    let (samples_l, samples_r) = match channels {
        1 => (samples.clone(), samples),
        2 => {
            // Interleaved → de-interleave.
            let mut l = Vec::with_capacity(samples.len() / 2);
            let mut r = Vec::with_capacity(samples.len() / 2);
            for chunk in samples.chunks_exact(2) {
                l.push(chunk[0]);
                r.push(chunk[1]);
            }
            (l, r)
        }
        n => anyhow::bail!("unsupported channel count: {n}"),
    };

    Ok(WavContent { samples_l, samples_r, channels, sample_rate })
}

/// Run the audio engine. Returns when the WAV finishes playing (or never,
/// if `looped`).
pub fn run(
    plugin: LoadedPlugin,
    wav: WavContent,
    block_override: Option<u32>,
    looped: bool,
) -> Result<()> {
    // ── Set up cpal ──
    let cpal_host = cpal::default_host();
    let device = cpal_host
        .default_output_device()
        .context("no default audio output device")?;
    eprintln!("sdsp-runner: output = '{}'", device.name().unwrap_or_default());
    let supported = device
        .default_output_config()
        .context("no default output config")?;
    let sr = supported.sample_rate().0;
    let cpal_channels = supported.channels() as usize;
    let block = block_override.unwrap_or(512);
    eprintln!(
        "sdsp-runner: cpal sr={sr} ch={cpal_channels} block={block}, sample format = {:?}",
        supported.sample_format()
    );

    // ── Instantiate the CLAP plugin ──
    let host_info = host_info()?;
    let mut instance = PluginInstance::<Host>::new(
        |_| HostShared,
        |_| (),
        &plugin.entry,
        &plugin.id_cstr(),
        &host_info,
    )
    .context("plugin instantiation failed")?;

    let cfg = PluginAudioConfiguration {
        sample_rate: sr as f64,
        min_frames_count: block,
        max_frames_count: block,
    };
    let stopped = instance
        .activate(|_, _| (), cfg)
        .context("plugin activate() failed")?;
    let mut started = stopped
        .start_processing()
        .map_err(|_| anyhow::anyhow!("plugin start_processing() failed"))?;

    // ── Shared playback state ──
    let pos = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let pos_clone = pos.clone();
    let done_clone = done.clone();

    // Pre-allocate buffers reused across audio callbacks.
    let mut in_l = vec![0.0_f32; block as usize];
    let mut in_r = vec![0.0_f32; block as usize];
    let mut out_l = vec![0.0_f32; block as usize];
    let mut out_r = vec![0.0_f32; block as usize];

    let mut input_ports = AudioPorts::with_capacity(2, 1);
    let mut output_ports = AudioPorts::with_capacity(2, 1);

    let total_frames = wav.frames();
    let samples_l = Arc::new(wav.samples_l);
    let samples_r = Arc::new(wav.samples_r);
    let sl = samples_l.clone();
    let sr_ = samples_r.clone();

    // ── cpal callback ──
    // We process in chunks of `block` frames inside one callback if the
    // callback's output buffer is bigger than that.
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => {
            let config: cpal::StreamConfig = supported.into();
            device
                .build_output_stream(
                    &config,
                    move |out: &mut [f32], _info: &cpal::OutputCallbackInfo| {
                        let mut frame_out = 0;
                        while frame_out < out.len() / cpal_channels {
                            let mut current_pos = pos_clone.load(Ordering::Relaxed);
                            // Fill the plugin input block from the wav.
                            let chunk = (block as usize).min(out.len() / cpal_channels - frame_out);
                            for i in 0..chunk {
                                let src = if looped {
                                    current_pos % total_frames
                                } else {
                                    current_pos
                                };
                                in_l[i] = sl.get(src).copied().unwrap_or(0.0);
                                in_r[i] = sr_.get(src).copied().unwrap_or(0.0);
                                current_pos += 1;
                            }
                            if !looped && current_pos >= total_frames {
                                done_clone.store(true, Ordering::Relaxed);
                            }
                            pos_clone.store(current_pos, Ordering::Relaxed);

                            // Run plugin process() — borrow the two-channel slices.
                            let in_chans: [&mut [f32]; 2] = [&mut in_l[..chunk], &mut in_r[..chunk]];
                            let input_audio = input_ports.with_input_buffers([AudioPortBuffer {
                                latency: 0,
                                channels: AudioPortBufferType::f32_input_only(
                                    in_chans.into_iter().map(InputChannel::variable),
                                ),
                            }]);
                            let mut out_chans: [&mut [f32]; 2] =
                                [&mut out_l[..chunk], &mut out_r[..chunk]];
                            let mut output_audio = output_ports.with_output_buffers([AudioPortBuffer {
                                latency: 0,
                                channels: AudioPortBufferType::f32_output_only(
                                    out_chans.iter_mut().map(|b| &mut **b),
                                ),
                            }]);
                            let evs: [clack_host::events::event_types::NoteOnEvent; 0] = [];
                            let input_events = InputEvents::from_buffer(&evs);
                            let mut out_evs = EventBuffer::new();
                            let mut output_events = OutputEvents::from_buffer(&mut out_evs);
                            let _ = started.process(
                                &input_audio,
                                &mut output_audio,
                                &input_events,
                                &mut output_events,
                                None,
                                None,
                            );

                            // Interleave plugin stereo into cpal output.
                            for i in 0..chunk {
                                let base = (frame_out + i) * cpal_channels;
                                out[base] = out_l[i];
                                if cpal_channels >= 2 {
                                    out[base + 1] = out_r[i];
                                }
                                // Fill any extra channels with silence.
                                for ch in 2..cpal_channels {
                                    out[base + ch] = 0.0;
                                }
                            }
                            frame_out += chunk;
                        }
                    },
                    |err| eprintln!("sdsp-runner: cpal stream error: {err}"),
                    None,
                )
                .context("build_output_stream failed")?
        }
        fmt => anyhow::bail!("unsupported cpal sample format: {fmt:?}"),
    };

    stream.play().context("stream.play() failed")?;

    // ── Wait for the WAV to finish (or Ctrl-C) ──
    if looped {
        eprintln!("sdsp-runner: looping forever — Ctrl-C to stop");
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    } else {
        while !done.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        // Drain one more second so the plugin's tail doesn't cut off.
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    drop(stream);
    Ok(())
}
