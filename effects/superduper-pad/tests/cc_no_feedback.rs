//! cc_no_feedback.rs — regression test for lesson 21b. MIDI CC#1
//! (ModWheel) writes into a CLAP param atomic but MUST NOT raise the
//! dirty_params bit; otherwise the plugin echoes the CC out as a
//! ParamValueEvent → host re-records it into the FX envelope → on the
//! next playback the envelope replays into the CC handler → runaway.
//!
//! Probe: send CC#1=64, then CC#1=127, then CC#1=0 in three blocks,
//! count ParamValueEvents in the output queue. Must be 0.
//!
//! Run: cargo test --release -p superduper-pad --test cc_no_feedback

use clack_common::events::Pckn;
use clack_common::events::event_types::{MidiEvent, ParamValueEvent};
use clack_common::utils::ClapId;
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, AudioPorts};
use clack_plugin::entry::SinglePluginEntry;
use superduper_pad::SuperDuperPad;

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

#[test]
fn cc_does_not_echo_param_value_events() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperPad>>(
        c"/in/process/test/superduper-pad-cc-feedback",
    ).expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TH>::new(
        |_| TS, |_| (), &entry, c"co.superduperai.pad", &host_info,
    ).expect("instantiate");

    const BLOCK: u32 = 256;
    let stopped = plugin.activate(|_, _| (), PluginAudioConfiguration {
        sample_rate: 48_000.0, min_frames_count: BLOCK, max_frames_count: BLOCK,
    }).expect("activate");

    let cc_values = [64u8, 127, 0, 100, 50];
    let mut total_param_events_out = 0usize;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(0, 0);
            let mut out_ports = AudioPorts::with_capacity(2, 1);

            for &v in cc_values.iter() {
                let mut in_buf = EventBuffer::new();
                let cc = MidiEvent::new(0, 0, [0xB0, 1, v]);
                in_buf.push(&cc);
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);

                let mut out_l = vec![0.0_f32; BLOCK as usize];
                let mut out_r = vec![0.0_f32; BLOCK as usize];

                let input_audio = in_ports.with_input_buffers(std::iter::empty::<
                    AudioPortBuffer<
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f32>>,
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f64>>,
                    >,
                >());
                let mut out_chans: [&mut [f32]; 2] = [out_l.as_mut_slice(), out_r.as_mut_slice()];
                let mut output_audio = out_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        out_chans.iter_mut().map(|b| &mut **b),
                    ),
                }]);

                proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None)
                    .expect("process");

                // Walk the output event buffer and count ParamValueEvents.
                for ev in out_evs.iter() {
                    if ev.as_event::<ParamValueEvent>().is_some() {
                        total_param_events_out += 1;
                    }
                }
            }
            proc.stop_processing()
        }).join().expect("audio thread")
    });
    plugin.deactivate(stopped_back);

    let _ = ClapId::new(0); // suppress unused-import nag

    eprintln!(
        "After {} CC#1 messages, plugin emitted {} ParamValueEvent(s)",
        cc_values.len(), total_param_events_out
    );
    assert_eq!(
        total_param_events_out, 0,
        "CC#1 is echoing as ParamValueEvent — this would create a runaway \
         loop with REAPER's FX envelope. See CLAUDE.md lesson 21b."
    );
}
