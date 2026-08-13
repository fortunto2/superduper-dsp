//! End-to-end CLAP integration test for SuperDuper Formant.
//!
//! Loads the plugin through `clack-host` (no REAPER), activates it, streams a
//! harmonically rich drone through the main input and asserts the output is
//! neither silent nor identical to the input. Catches CLAP-plumbing bugs
//! (audio-ports declaration, param routing) that `dsp_smoke` cannot see.
//!
//! Run: `cargo test --release -p superduper-formant --test clap_e2e -- --nocapture`

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_plugin::entry::SinglePluginEntry;

use superduper_formant::SuperDuperFormant;

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
    (samples.iter().map(|&x| x * x).sum::<f32>() / samples.len() as f32).sqrt()
}
fn peak(samples: &[f32]) -> f32 {
    samples.iter().map(|x| x.abs()).fold(0.0f32, f32::max)
}

#[test]
fn formant_clap_pipeline_produces_audio() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperFormant>>(
        c"/in/process/test/superduper-formant",
    )
    .expect("plugin entry should load");

    let factory = entry
        .get_factory::<clack_host::factory::plugin::PluginFactory>()
        .expect("plugin factory must be present");
    let desc = factory.plugin_descriptor(0).unwrap();
    assert_eq!(desc.id().unwrap().to_bytes(), b"co.superduperai.formant");

    let host_info =
        HostInfo::new("SuperDuper Test", "SuperDuperAI", "https://superduperai.co", "0.0.1")
            .unwrap();

    let mut plugin_instance = PluginInstance::<TestHost>::new(
        |_| TestHostShared,
        |_| (),
        &entry,
        c"co.superduperai.formant",
        &host_info,
    )
    .expect("plugin should instantiate");

    const SR: f32 = 48_000.0;
    const BLOCK: u32 = 512;
    let cfg = PluginAudioConfiguration {
        sample_rate: SR as f64,
        min_frames_count: BLOCK,
        max_frames_count: BLOCK,
    };
    let stopped_audio_proc = plugin_instance.activate(|_, _| (), cfg).expect("activate");

    // 1 s of a harmonically rich 110 Hz drone — the thing a formant filter bites on.
    const TOTAL_FRAMES: usize = SR as usize;
    const BLOCK_USIZE: usize = BLOCK as usize;
    let n_blocks = TOTAL_FRAMES / BLOCK_USIZE;

    let mut all_in_l: Vec<f32> = vec![0.0; TOTAL_FRAMES];
    let mut all_in_r: Vec<f32> = vec![0.0; TOTAL_FRAMES];
    for i in 0..TOTAL_FRAMES {
        let t = i as f32 / SR;
        let mut s = 0.0;
        for h in 1..=24 {
            s += (std::f32::consts::TAU * 110.0 * h as f32 * t).sin() / h as f32;
        }
        all_in_l[i] = s * 0.25;
        all_in_r[i] = s * 0.25;
    }
    let reference = all_in_l.clone();
    let mut all_out_l: Vec<f32> = vec![0.0; TOTAL_FRAMES];
    let mut all_out_r: Vec<f32> = vec![0.0; TOTAL_FRAMES];

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

    let tail = &all_out_l[TOTAL_FRAMES / 2..];
    let peak_l = peak(tail);
    let rms_l = rms(tail);
    eprintln!("CLAP e2e: tail peak={peak_l:.6} rms={rms_l:.6}");

    assert!(all_out_l.iter().all(|v| v.is_finite()), "output has NaN/Inf");
    assert!(
        peak_l > 1e-3,
        "output is silent through the CLAP pipeline (peak={peak_l}) — \
         process() never ran or audio-ports routing is broken",
    );
    // Default is Mix = 1 on a vowel, so the output must differ from the input.
    let max_dev = reference[TOTAL_FRAMES / 2..]
        .iter()
        .zip(tail.iter())
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_dev > 1e-3,
        "output is identical to input — params never reached the DSP (dev={max_dev})"
    );
}
