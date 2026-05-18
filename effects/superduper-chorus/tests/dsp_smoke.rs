//! Chorus smoke: feed a sine through, assert output is non-zero,
//! finite, and different from the input (modulation should add at
//! least some variation).

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel};
use clack_plugin::entry::SinglePluginEntry;
use superduper_chorus::SuperDuperChorus;

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
fn chorus_modulates_a_sine() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperChorus>>(
        c"/in/process/test/superduper-chorus-smoke",
    ).expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TH>::new(
        |_| TS, |_| (), &entry, c"co.superduperai.chorus", &host_info,
    ).expect("instantiate");

    const SR: f32 = 48_000.0;
    const BLOCK: u32 = 256;
    let total: usize = (SR * 1.5) as usize;
    let n_blocks = total / BLOCK as usize;
    let stopped = plugin.activate(|_, _| (), PluginAudioConfiguration {
        sample_rate: SR as f64, min_frames_count: BLOCK, max_frames_count: BLOCK,
    }).expect("activate");

    let signal: Vec<f32> = (0..total).map(|i| {
        let t = i as f32 / SR;
        0.4 * (t * 440.0 * std::f32::consts::TAU).sin()
    }).collect();
    let mut out_l = vec![0.0_f32; total];
    let mut out_r = vec![0.0_f32; total];
    let sref = &signal;
    let l_ref = &mut out_l;
    let r_ref = &mut out_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(2, 1);
            let mut out_ports = AudioPorts::with_capacity(2, 1);
            for block in 0..n_blocks {
                let start = block * BLOCK as usize;
                let end = start + BLOCK as usize;
                let in_buf = EventBuffer::new();
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);
                let mut chunk_l = sref[start..end].to_vec();
                let mut chunk_r = sref[start..end].to_vec();
                let chans = [
                    InputChannel::variable(chunk_l.as_mut_slice()),
                    InputChannel::variable(chunk_r.as_mut_slice()),
                ];
                let input_audio = in_ports.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(chans.into_iter()),
                }]);
                let lb = &mut l_ref[start..end];
                let rb = &mut r_ref[start..end];
                let mut out_chans: [&mut [f32]; 2] = [lb, rb];
                let mut output_audio = out_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(out_chans.iter_mut().map(|b| &mut **b)),
                }]);
                proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None)
                    .expect("process");
            }
            proc.stop_processing()
        }).join().expect("audio")
    });
    plugin.deactivate(stopped_back);

    let skip = (SR * 0.10) as usize;
    let rms_l: f32 = (out_l[skip..].iter().map(|x| x * x).sum::<f32>() / (out_l.len() - skip) as f32).sqrt();
    assert!(rms_l > 0.05, "chorus output suspiciously quiet (rms={rms_l})");
    for x in &out_l[skip..] { assert!(x.is_finite(), "NaN/Inf in chorus output"); }
    let peak = out_l.iter().fold(0.0_f32, |a, x| a.max(x.abs()));
    assert!(peak < 0.99, "chorus clipping at peak {peak}");
    // L vs R should differ slightly thanks to quadrature LFO.
    let diff: f32 = out_l[skip..].iter().zip(out_r[skip..].iter())
        .map(|(a, b)| (a - b).powi(2)).sum::<f32>().sqrt();
    eprintln!("Chorus: rms_l={:.4}, peak={:.4}, L-R diff={:.4}", rms_l, peak, diff);
    assert!(diff > 0.01, "L/R look identical — quadrature LFO not working");
}
