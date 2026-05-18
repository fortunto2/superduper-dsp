//! clap_e2e.rs — boot Ambient through clack-host with NO MIDI input
//! and verify it spontaneously produces audio. Ambient is the only
//! plugin that generates sound autonomously (no NoteOn required).
//!
//! Run: cargo test --release -p superduper-ambient --test clap_e2e -- --nocapture

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts,
};
use clack_plugin::entry::SinglePluginEntry;
use superduper_ambient::SuperDuperAmbient;

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
fn ambient_generates_audio_without_midi() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperAmbient>>(
        c"/in/process/test/superduper-ambient-e2e",
    ).expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TH>::new(
        |_| TS, |_| (), &entry, c"co.superduperai.ambient", &host_info,
    ).expect("instantiate");

    const SR: f32 = 48_000.0;
    const BLOCK: u32 = 256;
    const SECONDS: f32 = 4.0;
    let total_frames = (SR * SECONDS) as usize;
    let block_us = BLOCK as usize;
    let n_blocks = total_frames / block_us;

    let stopped = plugin.activate(|_, _| (), PluginAudioConfiguration {
        sample_rate: SR as f64, min_frames_count: BLOCK, max_frames_count: BLOCK,
    }).expect("activate");

    let mut all_l = vec![0.0_f32; total_frames];
    let mut all_r = vec![0.0_f32; total_frames];
    let l_ref = &mut all_l;
    let r_ref = &mut all_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(0, 0);
            let mut out_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let start = block * block_us;
                let end = start + block_us;
                let in_buf = EventBuffer::new();
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);

                let mut out_l = vec![0.0_f32; block_us];
                let mut out_r = vec![0.0_f32; block_us];

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

                l_ref[start..end].copy_from_slice(&out_l);
                r_ref[start..end].copy_from_slice(&out_r);
            }
            proc.stop_processing()
        }).join().expect("audio thread")
    });
    plugin.deactivate(stopped_back);

    // After 4 seconds of running with no input, Ambient should have
    // built up an audible drone. Skip the first half (envelope attack)
    // and compute RMS on the second half.
    let skip = all_l.len() / 2;
    let rms_l: f32 = (all_l[skip..].iter().map(|x| x * x).sum::<f32>()
        / (all_l.len() - skip) as f32).sqrt();
    let rms_r: f32 = (all_r[skip..].iter().map(|x| x * x).sum::<f32>()
        / (all_r.len() - skip) as f32).sqrt();
    let rms_db = 20.0 * 0.5 * (rms_l + rms_r).max(1e-9).log10();
    eprintln!("Ambient RMS after 4 s: L={:.4}, R={:.4} ({:.1} dBFS)", rms_l, rms_r, rms_db);

    assert!(
        rms_l > 0.001 && rms_r > 0.001,
        "Ambient produced silence — autonomous drone generator failed (L={rms_l}, R={rms_r})"
    );
    assert!(
        rms_l.is_finite() && rms_r.is_finite(),
        "Ambient produced NaN/Inf"
    );
    // Sanity — should be reasonably quiet (not blown out).
    let peak: f32 = all_l.iter().chain(all_r.iter()).fold(0.0, |a, x| a.max(x.abs()));
    assert!(peak < 0.99, "Ambient is clipping at peak {peak}");
}
