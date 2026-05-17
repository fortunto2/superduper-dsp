//! Smoke tests for SuperDuper Limiter. Verifies the limiter never lets the
//! ceiling be exceeded on the sample peak (true-peak is a different
//! guarantee and depends on the upsampler, which we test separately).

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_plugin::entry::SinglePluginEntry;
use superduper_limiter::SuperDuperLimiter;

struct HS;
impl SharedHandler<'_> for HS {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}
impl HostLogImpl for HS {
    fn log(&self, sev: LogSeverity, msg: &str) { eprintln!("[{sev}] {msg}"); }
}
struct H;
impl HostHandlers for H {
    type Shared<'a> = HS;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
    fn declare_extensions(builder: &mut HostExtensions<Self>, _: &Self::Shared<'_>) {
        builder.register::<HostLog>();
    }
}

#[test]
fn limiter_holds_ceiling() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperLimiter>>(
        c"/test/limiter",
    ).unwrap();
    let host_info = HostInfo::new("Test", "SDA", "https://t", "0").unwrap();
    let mut inst = PluginInstance::<H>::new(
        |_| HS, |_| (), &entry, c"co.superduperai.limiter", &host_info,
    ).unwrap();

    const SR: f32 = 48_000.0;
    const BLOCK: u32 = 512;
    let cfg = PluginAudioConfiguration {
        sample_rate: SR as f64,
        min_frames_count: BLOCK,
        max_frames_count: BLOCK,
    };
    let stopped = inst.activate(|_, _| (), cfg).unwrap();

    // Feed huge sine wave; ceiling default = -0.3 dB.
    const N: usize = SR as usize;
    let mut in_l = vec![0.0_f32; N];
    let mut in_r = vec![0.0_f32; N];
    for i in 0..N {
        let p = i as f32 * 2.0 * core::f32::consts::PI * 220.0 / SR;
        let v = p.sin() * 2.0; // +6 dB over unity
        in_l[i] = v;
        in_r[i] = v;
    }
    let mut out_l = vec![0.0_f32; N];
    let mut out_r = vec![0.0_f32; N];

    let il = &mut in_l; let ir = &mut in_r;
    let ol = &mut out_l; let or_ = &mut out_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut ap = stopped.start_processing().unwrap();
            let mut input_ports = AudioPorts::with_capacity(2, 1);
            let mut output_ports = AudioPorts::with_capacity(2, 1);
            let n_blocks = N / BLOCK as usize;
            for b in 0..n_blocks {
                let st = b * BLOCK as usize;
                let en = st + BLOCK as usize;
                let il_chunk = &mut il[st..en];
                let ir_chunk = &mut ir[st..en];
                let mut in_chans: [&mut [f32]; 2] = [il_chunk, ir_chunk];
                let mut ol_chunk = vec![0.0_f32; BLOCK as usize];
                let mut or_chunk = vec![0.0_f32; BLOCK as usize];
                let evs: [clack_host::events::event_types::NoteOnEvent; 0] = [];
                let input_events = InputEvents::from_buffer(&evs);
                let mut out_evs = EventBuffer::new();
                let mut output_events = OutputEvents::from_buffer(&mut out_evs);
                let input_audio = input_ports.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        in_chans.iter_mut().map(|b| InputChannel::variable(*b)),
                    ),
                }]);
                let mut out_chans: [&mut [f32]; 2] = [ol_chunk.as_mut_slice(), or_chunk.as_mut_slice()];
                let mut output_audio = output_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        out_chans.iter_mut().map(|b| &mut **b),
                    ),
                }]);
                ap.process(&input_audio, &mut output_audio, &input_events,
                    &mut output_events, None, None).unwrap();
                ol[st..en].copy_from_slice(&ol_chunk);
                or_[st..en].copy_from_slice(&or_chunk);
            }
            ap.stop_processing()
        }).join().unwrap()
    });
    inst.deactivate(stopped_back);

    // Default ceiling = -0.3 dB ≈ 0.9661. Allow 1% tolerance for transient
    // lookahead skew.
    let ceiling = 10f32.powf(-0.3 / 20.0);
    // Skip first 500 samples (lookahead settling).
    let peak_l = out_l[500..].iter().map(|x| x.abs()).fold(0.0_f32, f32::max);
    let peak_r = out_r[500..].iter().map(|x| x.abs()).fold(0.0_f32, f32::max);
    eprintln!("limiter peak L={peak_l:.4} R={peak_r:.4} ceiling={ceiling:.4}");
    assert!(peak_l <= ceiling * 1.01, "L over ceiling: {peak_l} vs {ceiling}");
    assert!(peak_r <= ceiling * 1.01, "R over ceiling: {peak_r} vs {ceiling}");
}
