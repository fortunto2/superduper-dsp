//! clap-validator says the Looper runs 3–21× slower on subnormal input. Its
//! test also sends a fresh draw of random note / MIDI events in the second
//! pass, and the Looper turns MIDI CCs into record / overdub / play, so the
//! two passes may do different work. This measures the same thing without
//! events: white noise against subnormals through a real CLAP host, the
//! FTZ guard in place. If this stays near 1× the validator's ratio is the
//! events, not denormals.

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::event_types::MidiEvent;
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel};
use clack_plugin::entry::SinglePluginEntry;
use std::time::Instant;
use superduper_looper::SuperDuperLooper;

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
    fn declare_extensions(b: &mut HostExtensions<Self>, _: &Self::Shared<'_>) { b.register::<HostLog>(); }
}

const BLOCK: u32 = 512;
const BLOCKS: usize = 400;

/// Seconds to process `BLOCKS` blocks of `sample()` input. `cc` = (block,
/// controller) MIDI CCs to send at value 127: 20 = Rec track 1, 24 =
/// Play/Stop, 28 = Overdub — what clap-validator's random MIDI also does.
fn time_pass(sample: impl Fn(u32) -> f32 + Send, cc: &[(usize, u8)]) -> f64 {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperLooper>>(c"/in/process/test/superduper-looper-denormals").expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TH>::new(|_| TS, |_| (), &entry, c"co.superduperai.looper", &host_info).expect("instantiate");
    let stopped = plugin.activate(|_, _| (), PluginAudioConfiguration { sample_rate: 48_000.0, min_frames_count: BLOCK, max_frames_count: BLOCK }).expect("activate");
    let mut seconds = 0.0;
    let s_ref = &mut seconds;
    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(2, 1);
            let mut out_ports = AudioPorts::with_capacity(2, 1);
            let mut seed = 0x1234_5678u32;
            let t0 = Instant::now();
            for block in 0..BLOCKS {
                let mut in_buf = EventBuffer::new();
                for &(at, controller) in cc {
                    if at == block {
                        in_buf.push(&MidiEvent::new(0, 0, [0xB0, controller, 127]));
                    }
                }
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);
                let mut inl = vec![0.0f32; BLOCK as usize];
                let mut inr = vec![0.0f32; BLOCK as usize];
                for i in 0..BLOCK as usize {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    inl[i] = sample(seed);
                    inr[i] = sample(seed.rotate_left(7));
                }
                let (mut ol, mut or) = (vec![0.0f32; BLOCK as usize], vec![0.0f32; BLOCK as usize]);
                let input_audio = in_ports.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only([InputChannel::constant(&mut inl), InputChannel::constant(&mut inr)].into_iter()),
                }]);
                let mut chans: [&mut [f32]; 2] = [ol.as_mut_slice(), or.as_mut_slice()];
                let mut output_audio = out_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(chans.iter_mut().map(|b| &mut **b)),
                }]);
                proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None).expect("process");
                assert!(ol.iter().all(|x| x.is_finite()));
            }
            *s_ref = t0.elapsed().as_secs_f64();
            proc.stop_processing()
        }).join().expect("audio")
    });
    plugin.deactivate(stopped_back);
    seconds
}

#[test]
fn subnormal_input_costs_the_same_as_noise() {
    let noise = |s: u32| (s >> 8) as f32 / 8_388_608.0 - 1.0;
    let subnormal = |s: u32| f32::from_bits(s & 0x007f_ffff); // every value below f32::MIN_POSITIVE
    let _warm = time_pass(noise, &[]);
    let t_noise = time_pass(noise, &[]);
    let t_sub = time_pass(subnormal, &[]);
    let ratio = t_sub / t_noise;
    eprintln!("looper idle: noise {:.1} ms, subnormals {:.1} ms, ratio {ratio:.2}", t_noise * 1e3, t_sub * 1e3);
    assert!(ratio < 2.0, "subnormal input is {ratio:.2}× slower — a denormal path with no FTZ");
}

#[test]
fn subnormal_input_costs_the_same_while_recording_and_overdubbing() {
    let noise = |s: u32| (s >> 8) as f32 / 8_388_608.0 - 1.0;
    let subnormal = |s: u32| f32::from_bits(s & 0x007f_ffff);
    // Rec at block 0, Play/Stop at 100 (loop closes), Overdub at 120 (adds
    // onto the loop while playing), Play/Stop at 300.
    let cc = [(0usize, 20u8), (100, 24), (120, 28), (300, 24)];
    let _warm = time_pass(noise, &cc);
    let t_noise = time_pass(noise, &cc);
    let t_sub = time_pass(subnormal, &cc);
    let ratio = t_sub / t_noise;
    eprintln!("looper rec/overdub: noise {:.1} ms, subnormals {:.1} ms, ratio {ratio:.2}", t_noise * 1e3, t_sub * 1e3);
    assert!(ratio < 2.0, "subnormal input is {ratio:.2}× slower while looping — a denormal path with no FTZ");
}
