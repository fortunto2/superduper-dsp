//! End-to-end CLAP integration test for SuperDuper Reverb.
//!
//! Drives the plugin through the full CLAP pipeline using `clack-host`
//! as the host, without REAPER or any DAW. This isolates whether bugs
//! live in:
//!   - DSP code (covered by `dsp_smoke.rs`)
//!   - CLAP plumbing (audio-ports declaration, process() routing,
//!     parameter event handling) — covered HERE
//!   - REAPER caching/scanning quirks — anything that fails in REAPER
//!     but passes here is host-side.
//!
//! Run with:
//!     cargo test -p superduper-reverb --test clap_e2e -- --nocapture

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_plugin::entry::SinglePluginEntry;

use superduper_reverb::SuperDuperReverb;

struct TestHostShared;

impl SharedHandler<'_> for TestHostShared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}

impl HostLogImpl for TestHostShared {
    fn log(&self, severity: LogSeverity, message: &str) {
        eprintln!("[plugin {severity}] {message}");
    }
}

struct TestHost;

impl HostHandlers for TestHost {
    type Shared<'a> = TestHostShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();

    fn declare_extensions(builder: &mut HostExtensions<Self>, _: &Self::Shared<'_>) {
        builder.register::<HostLog>();
    }
}

fn rms(samples: &[f32]) -> f32 {
    let sum_sq: f32 = samples.iter().map(|&x| x * x).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().map(|x| x.abs()).fold(0.0f32, f32::max)
}

#[test]
fn reverb_clap_pipeline_modifies_signal() {
    // Step 1: load the plugin via clack-plugin entry (in-process, no dylib).
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperReverb>>(
        c"/in/process/test/superduper-reverb",
    )
    .expect("plugin entry should load");

    // Sanity-check the descriptor that REAPER would see.
    let factory = entry
        .get_factory::<clack_host::factory::plugin::PluginFactory>()
        .expect("plugin factory must be present");
    let desc = factory.plugin_descriptor(0).unwrap();
    assert_eq!(desc.id().unwrap().to_bytes(), b"co.superduperai.reverb");

    let host_info =
        HostInfo::new("SuperDuper Test", "SuperDuperAI", "https://superduperai.co", "0.0.1")
            .unwrap();

    // Step 2: instantiate.
    let mut plugin_instance = PluginInstance::<TestHost>::new(
        |_| TestHostShared,
        |_| (),
        &entry,
        c"co.superduperai.reverb",
        &host_info,
    )
    .expect("plugin should instantiate");

    // Step 3: activate at REAPER-typical SR/buffer.
    const SR: f32 = 48_000.0;
    const BLOCK: u32 = 512;
    let cfg = PluginAudioConfiguration {
        sample_rate: SR as f64,
        min_frames_count: BLOCK,
        max_frames_count: BLOCK,
    };
    let stopped_audio_proc = plugin_instance
        .activate(|_, _| (), cfg)
        .expect("activate");

    // Step 4: prepare stereo buffers — feed a short impulse, then silence.
    // Total length = several seconds so the reverb tail is observable.
    const TOTAL_FRAMES: usize = (SR as usize) * 2; // 2 seconds
    const BLOCK_USIZE: usize = BLOCK as usize;
    let n_blocks = TOTAL_FRAMES / BLOCK_USIZE;

    let mut all_in_l: Vec<f32> = vec![0.0; TOTAL_FRAMES];
    let mut all_in_r: Vec<f32> = vec![0.0; TOTAL_FRAMES];
    // A loud impulse cluster at the start
    for s in all_in_l.iter_mut().take(8) {
        *s = 1.0;
    }
    for s in all_in_r.iter_mut().take(8) {
        *s = 1.0;
    }
    let mut all_out_l: Vec<f32> = vec![0.0; TOTAL_FRAMES];
    let mut all_out_r: Vec<f32> = vec![0.0; TOTAL_FRAMES];

    // Move the audio processor onto a separate "audio" thread (just our thread,
    // but the API enforces start_processing in audio-thread context).
    let out_l_ref = &mut all_out_l;
    let out_r_ref = &mut all_out_r;
    let in_l_ref = &mut all_in_l;
    let in_r_ref = &mut all_in_r;
    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut audio_proc = stopped_audio_proc
                .start_processing()
                .expect("start_processing");

            let mut input_ports = AudioPorts::with_capacity(2, 1);
            let mut output_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let start = block * BLOCK_USIZE;
                let end = start + BLOCK_USIZE;

                let in_l_chunk = &mut in_l_ref[start..end];
                let in_r_chunk = &mut in_r_ref[start..end];
                let mut in_chans: [&mut [f32]; 2] = [in_l_chunk, in_r_chunk];

                let mut out_l_chunk = vec![0.0_f32; BLOCK_USIZE];
                let mut out_r_chunk = vec![0.0_f32; BLOCK_USIZE];

                let input_events_buf: [clack_host::events::event_types::NoteOnEvent; 0] = [];
                let input_events = InputEvents::from_buffer(&input_events_buf);
                let mut output_events_buf = EventBuffer::new();
                let mut output_events = OutputEvents::from_buffer(&mut output_events_buf);

                let input_audio = input_ports.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        in_chans.iter_mut().map(|b| InputChannel::variable(*b)),
                    ),
                }]);

                let mut out_chans: [&mut [f32]; 2] =
                    [out_l_chunk.as_mut_slice(), out_r_chunk.as_mut_slice()];
                let mut output_audio = output_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        out_chans.iter_mut().map(|b| &mut **b),
                    ),
                }]);

                audio_proc
                    .process(
                        &input_audio,
                        &mut output_audio,
                        &input_events,
                        &mut output_events,
                        None,
                        None,
                    )
                    .expect("process should succeed");

                out_l_ref[start..end].copy_from_slice(&out_l_chunk);
                out_r_ref[start..end].copy_from_slice(&out_r_chunk);
            }

            audio_proc.stop_processing()
        })
        .join()
        .expect("audio thread")
    });

    plugin_instance.deactivate(stopped_back);

    // Step 5: analyse.
    // Tail = everything AFTER the impulse cluster (sample 100 onwards).
    let tail_l = &all_out_l[100..];
    let tail_r = &all_out_r[100..];

    let peak_l = peak(tail_l);
    let peak_r = peak(tail_r);
    let rms_l = rms(tail_l);
    let rms_r = rms(tail_r);

    eprintln!("CLAP e2e: tail peak L={:.6} R={:.6}", peak_l, peak_r);
    eprintln!("CLAP e2e: tail rms  L={:.6} R={:.6}", rms_l, rms_r);
    eprintln!("first 32 output samples L:");
    for (i, v) in all_out_l[..32].iter().enumerate() {
        eprintln!("  [{i}] {v:+.6}");
    }

    assert!(
        peak_l > 1e-4 || peak_r > 1e-4,
        "reverb tail is silent through CLAP pipeline (peak L={peak_l}, R={peak_r}) — \
         either process() never got called or audio-ports routing is broken",
    );
}
