//! Spectrum is a pass-through analyser — DSP correctness means
//! the output equals the input (sample-for-sample identity), and
//! the audio-thread ring buffer captures samples for the GUI without
//! crashing under typical block sizes.
//!
//! Run: cargo test --release -p superduper-spectrum --test dsp_smoke

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_plugin::entry::SinglePluginEntry;
use superduper_spectrum::SuperDuperSpectrum;

struct TestShared;
impl SharedHandler<'_> for TestShared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}
impl HostLogImpl for TestShared {
    fn log(&self, _: LogSeverity, _: &str) {}
}
struct TestHost;
impl HostHandlers for TestHost {
    type Shared<'a> = TestShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
    fn declare_extensions(b: &mut HostExtensions<Self>, _: &Self::Shared<'_>) {
        b.register::<HostLog>();
    }
}

#[test]
fn spectrum_is_sample_for_sample_passthrough() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperSpectrum>>(
        c"/in/process/test/superduper-spectrum",
    )
    .expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TestHost>::new(
        |_| TestShared, |_| (), &entry, c"co.superduperai.spectrum", &host_info,
    ).expect("instantiate");

    const SR: f64 = 48_000.0;
    const BLOCK: u32 = 256;
    const N_BLOCKS: usize = 12;

    let stopped = plugin.activate(|_, _| (), PluginAudioConfiguration {
        sample_rate: SR, min_frames_count: BLOCK, max_frames_count: BLOCK,
    }).expect("activate");

    // Generate a deterministic input — sine + small DC offset so we
    // can verify identity rather than silence-equals-silence.
    let block_us = BLOCK as usize;
    let total = block_us * N_BLOCKS;
    let mut in_l: Vec<f32> = (0..total).map(|i| {
        let t = i as f32 / SR as f32;
        (t * 440.0 * std::f32::consts::TAU).sin() * 0.5 + 0.1
    }).collect();
    let mut in_r: Vec<f32> = (0..total).map(|i| {
        let t = i as f32 / SR as f32;
        (t * 660.0 * std::f32::consts::TAU).sin() * 0.5 - 0.1
    }).collect();
    let mut out_l = vec![0.0_f32; total];
    let mut out_r = vec![0.0_f32; total];

    let in_l_orig = in_l.clone();
    let in_r_orig = in_r.clone();
    let out_l_ref = &mut out_l;
    let out_r_ref = &mut out_r;
    let in_l_mut = &mut in_l;
    let in_r_mut = &mut in_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(2, 1);
            let mut out_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..N_BLOCKS {
                let start = block * block_us;
                let end = start + block_us;
                let in_buf = EventBuffer::new();
                let mut out_evs = EventBuffer::new();
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);

                let mut in_l_chunk = in_l_mut[start..end].to_vec();
                let mut in_r_chunk = in_r_mut[start..end].to_vec();
                let in_chans = [
                    InputChannel::variable(in_l_chunk.as_mut_slice()),
                    InputChannel::variable(in_r_chunk.as_mut_slice()),
                ];
                let input_audio = in_ports.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(in_chans.into_iter()),
                }]);

                let l_buf = &mut out_l_ref[start..end];
                let r_buf = &mut out_r_ref[start..end];
                let mut out_chans: [&mut [f32]; 2] = [l_buf, r_buf];
                let mut output_audio = out_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        out_chans.iter_mut().map(|b| &mut **b),
                    ),
                }]);

                proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None)
                    .expect("process");
            }
            proc.stop_processing()
        }).join().expect("audio thread")
    });
    plugin.deactivate(stopped_back);

    // Identity check — every sample must match input.
    let mut max_diff = 0.0_f32;
    for i in 0..total {
        max_diff = max_diff.max((out_l[i] - in_l_orig[i]).abs());
        max_diff = max_diff.max((out_r[i] - in_r_orig[i]).abs());
    }
    assert!(
        max_diff < 1e-6,
        "Spectrum is supposed to be sample-for-sample pass-through; \
         max |out - in| = {max_diff}"
    );
}
