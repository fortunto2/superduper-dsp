//! clap-validator `process-varying-sample-rates` found a `-inf` sample at a
//! 1234.57 Hz sample rate (index 179, 2026-10-21). A host may ask for any
//! rate; the output must stay finite at all of them. Through a real CLAP
//! host, default parameters, every key struck once at velocities down to 0.

use clack_common::events::Pckn;
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::event_types::NoteOnEvent;
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, AudioPorts};
use clack_plugin::entry::SinglePluginEntry;
use superduper_drum::SuperDuperDrum;

struct TS;
impl SharedHandler<'_> for TS {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}
impl HostLogImpl for TS { fn log(&self, _: LogSeverity, _: &str) {} }
struct TH;
impl HostHandlers for TH {
    type Shared<'a> = TS;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
    fn declare_extensions(b: &mut HostExtensions<Self>, _: &Self::Shared<'_>) {
        b.register::<HostLog>();
    }
}

/// Renders one second with the pads struck at t=0; returns the first
/// non-finite sample index, if any, with the value.
fn first_non_finite(sr: f64, with_notes: bool) -> Option<(usize, f32)> {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperDrum>>(
        c"/in/process/test/superduper-drum-odd-sr",
    ).expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TH>::new(
        |_| TS, |_| (), &entry, c"co.superduperai.drum", &host_info,
    ).expect("instantiate");
    const BLOCK: u32 = 64;
    let n_blocks = (sr as usize) / BLOCK as usize + 1;
    let stopped = plugin.activate(|_, _| (), PluginAudioConfiguration {
        sample_rate: sr, min_frames_count: BLOCK, max_frames_count: BLOCK,
    }).expect("activate");
    let mut found = None;
    let found_ref = &mut found;
    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(0, 0);
            let mut out_ports = AudioPorts::with_capacity(2, 1);
            'blocks: for block in 0..n_blocks {
                let mut in_buf = EventBuffer::new();
                // Every key the host could send, at a spread of velocities
                // down to 0 — clap-validator's note generator does that, and
                // a struck pad with a 0 velocity is where a log of 0 hides.
                if with_notes && block < 128 {
                    let key = block as u16;
                    let vel = [0.9, 0.0, 0.001, 0.5, 1.0][block % 5];
                    in_buf.push(&NoteOnEvent::new(0, Pckn::new(0u16, 0u16, key, 0u32), vel));
                }
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);
                let mut l = vec![0.0_f32; BLOCK as usize];
                let mut r = vec![0.0_f32; BLOCK as usize];
                let input_audio = in_ports.with_input_buffers(std::iter::empty::<
                    AudioPortBuffer<
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f32>>,
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f64>>,
                    >,
                >());
                let mut chans: [&mut [f32]; 2] = [l.as_mut_slice(), r.as_mut_slice()];
                let mut output_audio = out_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(chans.iter_mut().map(|b| &mut **b)),
                }]);
                proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None).expect("process");
                for (i, x) in l.iter().chain(r.iter()).enumerate() {
                    if !x.is_finite() {
                        *found_ref = Some((block * BLOCK as usize + i % BLOCK as usize, *x));
                        break 'blocks;
                    }
                }
            }
            proc.stop_processing()
        }).join().expect("audio")
    });
    plugin.deactivate(stopped_back);
    found
}

#[test]
fn drum_stays_finite_at_every_sample_rate() {
    for sr in [1234.57, 8000.0, 22050.0, 44100.0, 96000.0, 192000.0] {
        for notes in [false, true] {
            if let Some((i, x)) = first_non_finite(sr, notes) {
                panic!("sr {sr} notes={notes}: sample {i} is {x}");
            }
        }
    }
}
