//! Through a real CLAP host: a 2-channel input port paired with the
//! 12-channel speaker port. The default placement (front, 7.1) lands on the
//! centre speaker alone once Width is 0 (at Width 1 a stereo input sits
//! ±15° around the position and leaks into the neighbours by design);
//! Monitor folds everything onto channels 1–2.

use clack_common::events::event_types::ParamValueEvent;
use clack_common::events::Pckn;
use clack_common::utils::{ClapId, Cookie};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel};
use clack_plugin::entry::SinglePluginEntry;
use superduper_space::{SuperDuperSpace, P_MONITOR, P_WIDTH, P_X, P_Y};

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

const SR: f64 = 48_000.0;
const BLOCK: u32 = 64;
const OUT: usize = 12;

/// Energy per output channel after `blocks` blocks of a 0.5 sine on both
/// inputs, with `params` sent in the first block.
fn render(params: &[(usize, f64)], blocks: usize) -> [f32; OUT] {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperSpace>>(c"/in/process/test/superduper-space").expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TH>::new(|_| TS, |_| (), &entry, c"co.superduperai.space", &host_info).expect("instantiate");
    let stopped = plugin.activate(|_, _| (), PluginAudioConfiguration { sample_rate: SR, min_frames_count: BLOCK, max_frames_count: BLOCK }).expect("activate");
    let mut energy = [0.0f32; OUT];
    let e_ref = &mut energy;
    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(2, 1);
            let mut out_ports = AudioPorts::with_capacity(OUT, 1);
            let mut phase = 0.0f32;
            for block in 0..blocks {
                let mut in_buf = EventBuffer::new();
                if block == 0 {
                    for &(id, v) in params {
                        in_buf.push(&ParamValueEvent::new(0, ClapId::new(id as u32), Pckn::new(0u16, 0u16, 0u16, 0u32), v, Cookie::empty()));
                    }
                }
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);
                let mut inp = vec![0.0f32; BLOCK as usize];
                for x in inp.iter_mut() { *x = 0.5 * phase.sin(); phase += 2.0 * core::f32::consts::PI * 440.0 / SR as f32; }
                let (mut inl, mut inr) = (inp.clone(), inp.clone());
                let mut outs: Vec<Vec<f32>> = (0..OUT).map(|_| vec![0.0f32; BLOCK as usize]).collect();
                let input_audio = in_ports.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only([InputChannel::constant(&mut inl), InputChannel::constant(&mut inr)].into_iter()),
                }]);
                let mut out_chans: Vec<&mut [f32]> = outs.iter_mut().map(|v| v.as_mut_slice()).collect();
                let mut output_audio = out_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(out_chans.iter_mut().map(|b| &mut **b)),
                }]);
                proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None).expect("process");
                if block + 1 == blocks {
                    for (c, o) in outs.iter().enumerate() { e_ref[c] = o.iter().map(|x| x * x).sum::<f32>() / o.len() as f32; }
                }
            }
            proc.stop_processing()
        }).join().expect("audio")
    });
    plugin.deactivate(stopped_back);
    energy
}

fn db(e: f32) -> f32 { 10.0 * e.max(1e-12).log10() }

#[test]
fn front_centre_lands_on_the_centre_speaker_alone() {
    let e = render(&[(P_WIDTH, 0.0)], 40);
    assert!(db(e[2]) > -15.0, "centre carries the signal: {e:?}");
    for (c, v) in e.iter().enumerate() {
        if c != 2 { assert!(db(*v) < -60.0, "channel {c} must be silent: {} dB", db(*v)); }
    }
}

#[test]
fn hard_right_lands_on_the_right_speaker_and_monitor_folds_to_the_first_two() {
    let e = render(&[(P_X, 1.0), (P_Y, 0.0), (P_WIDTH, 0.0)], 40); // azimuth 90: Rs on 7.1
    assert!(db(e[5]) > -15.0 && db(e[1]) < -60.0 && db(e[2]) < -60.0, "{e:?}");
    let f = render(&[(P_X, 1.0), (P_Y, 0.0), (P_WIDTH, 0.0), (P_MONITOR, 1.0)], 40);
    assert!(db(f[1]) > -15.0 && db(f[0]) < -40.0, "fold: Rs folds right: {f:?}");
    for c in 2..OUT { assert!(db(f[c]) < -60.0, "monitor leaves channel {c} silent: {f:?}"); }
}

#[test]
fn width_spreads_a_stereo_input_around_the_position() {
    // Width 1 at front centre: L sits at −15° between L and C, R at +15°
    // between C and R — the centre still carries most, the sides some.
    let e = render(&[], 40);
    assert!(db(e[2]) > db(e[0]) && db(e[2]) > db(e[1]), "{e:?}");
    assert!(db(e[0]) > -30.0 && db(e[1]) > -30.0, "the sides carry the image: {e:?}");
    assert!((db(e[0]) - db(e[1])).abs() < 0.5, "symmetric: {e:?}");
}
