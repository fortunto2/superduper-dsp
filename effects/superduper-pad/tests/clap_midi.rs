//! End-to-end CLAP test for Pad: feed a NoteOn through the host event
//! pipeline and verify the plugin's audio output is non-silent.
//!
//! If this passes but REAPER hears nothing, the bug is in note-port
//! advertisement or host routing. If this fails, the DSP path is broken.

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::events::Pckn;
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, AudioPorts};
use clack_plugin::entry::SinglePluginEntry;

use superduper_pad::SuperDuperPad;

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

fn peak(samples: &[f32]) -> f32 {
    samples.iter().map(|x| x.abs()).fold(0.0f32, f32::max)
}

fn rms(samples: &[f32]) -> f32 {
    let sum_sq: f32 = samples.iter().map(|&x| x * x).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

#[test]
fn note_on_produces_audible_output() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperPad>>(
        c"/in/process/test/superduper-pad",
    )
    .expect("plugin entry should load");

    let factory = entry
        .get_factory::<clack_host::factory::plugin::PluginFactory>()
        .expect("plugin factory must be present");
    let desc = factory.plugin_descriptor(0).unwrap();
    assert_eq!(desc.id().unwrap().to_bytes(), b"co.superduperai.pad");

    let host_info =
        HostInfo::new("SuperDuper Test", "SuperDuperAI", "https://superduperai.co", "0.0.1")
            .unwrap();

    let mut plugin_instance = PluginInstance::<TestHost>::new(
        |_| TestHostShared,
        |_| (),
        &entry,
        c"co.superduperai.pad",
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

    // Pad is a generator — no input ports, 1 stereo output port.
    const TOTAL_FRAMES: usize = (SR as usize) * 2; // 2 seconds
    const BLOCK_USIZE: usize = BLOCK as usize;
    let n_blocks = TOTAL_FRAMES / BLOCK_USIZE;
    let mut all_out_l: Vec<f32> = vec![0.0; TOTAL_FRAMES];
    let mut all_out_r: Vec<f32> = vec![0.0; TOTAL_FRAMES];

    let out_l_ref = &mut all_out_l;
    let out_r_ref = &mut all_out_r;
    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut audio_proc = stopped_audio_proc
                .start_processing()
                .expect("start_processing");

            // 0 input ports, 1 output port.
            let mut input_ports = AudioPorts::with_capacity(0, 0);
            let mut output_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let start = block * BLOCK_USIZE;
                let end = start + BLOCK_USIZE;

                // Build event buffer for this block.
                let mut input_buf = EventBuffer::new();
                if block == 0 {
                    // NoteOn at sample 0: MIDI key 60 (C4), full velocity.
                    let pckn = Pckn::new(0u16, 0u16, 60u16, 0u32);
                    let note_on = NoteOnEvent::new(0, pckn, 1.0);
                    input_buf.push(&note_on);
                } else if block == n_blocks - 4 {
                    // NoteOff a bit before end so we see release tail too.
                    let pckn = Pckn::new(0u16, 0u16, 60u16, 0u32);
                    let note_off = NoteOffEvent::new(0, pckn, 1.0);
                    input_buf.push(&note_off);
                }
                let input_events = InputEvents::from_buffer(&input_buf);
                if block == 0 {
                    eprintln!(
                        "BLOCK0 buf len={} input_events len={}",
                        input_buf.len(),
                        input_events.len()
                    );
                }
                let mut out_evs = EventBuffer::new();
                let mut output_events = OutputEvents::from_buffer(&mut out_evs);

                let mut out_l_chunk = vec![0.0_f32; BLOCK_USIZE];
                let mut out_r_chunk = vec![0.0_f32; BLOCK_USIZE];

                // Generator — empty input buffer array.
                let input_audio = input_ports
                    .with_input_buffers(std::iter::empty::<AudioPortBuffer<
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f32>>,
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f64>>,
                    >>());

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

    // Skip the first few blocks (envelope attack ramp-up) and look at the
    // sustained section.
    let warmup = (SR as usize) / 4; // 250 ms
    let cooldown = (SR as usize) / 4;
    let tail_l = &all_out_l[warmup..(TOTAL_FRAMES - cooldown)];
    let tail_r = &all_out_r[warmup..(TOTAL_FRAMES - cooldown)];
    let peak_l = peak(tail_l);
    let peak_r = peak(tail_r);
    let rms_l = rms(tail_l);
    let rms_r = rms(tail_r);
    eprintln!("Pad CLAP+MIDI: peak L={peak_l:.4} R={peak_r:.4}  rms L={rms_l:.4} R={rms_r:.4}");

    assert!(
        peak_l > 1e-3 || peak_r > 1e-3,
        "Pad must produce audio on NoteOn through CLAP (peak L={peak_l}, R={peak_r}) — \
         note port wiring or note-event handling is broken"
    );
}
