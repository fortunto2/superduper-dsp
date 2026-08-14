//! Render harness (NOT an assertion test): boots SuperDuper Wind on its default
//! Kurai preset, plays the Odysseus theme (C<->C# motif), and writes interleaved
//! f32 stereo to /tmp/wind_theme.f32 so it can be auditioned.
//! Run: cargo test --release -p superduper-wind --test render_theme -- --nocapture

use clack_common::events::event_types::ParamValueEvent;
use clack_common::events::Pckn;
use clack_common::utils::{ClapId, Cookie};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_plugin::entry::SinglePluginEntry;
use std::io::Write;

use superduper_wind::SuperDuperWind;

struct TS;
impl SharedHandler<'_> for TS {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}
impl HostLogImpl for TS {
    fn log(&self, severity: LogSeverity, message: &str) {
        eprintln!("[plugin {severity}] {message}");
    }
}
struct TH;
impl HostHandlers for TH {
    type Shared<'a> = TS;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
    fn declare_extensions(b: &mut HostExtensions<Self>, _: &Self::Shared<'_>) {
        b.register::<HostLog>();
    }
}

const SR: f32 = 48_000.0;
const BLOCK: u32 = 256;

// (pitch, start_s, dur_s, vel) — the Odysseus theme
const THEME: &[(u16, f32, f32, u8)] = &[
    (49, 0.02, 2.63, 115), (60, 2.72, 1.81, 79), (48, 4.60, 2.60, 97),
    (60, 7.27, 1.74, 87), (49, 9.08, 2.58, 115), (60, 11.73, 1.76, 75),
    (48, 13.56, 2.65, 105), (60, 16.28, 2.11, 96), (61, 18.41, 1.49, 91),
    (49, 19.97, 0.74, 102), (60, 20.78, 1.74, 80), (48, 22.59, 2.39, 97),
];

#[test]
fn render_odyssey_theme() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperWind>>(
        c"/in/process/render/wind-theme",
    )
    .expect("entry");
    let host_info =
        HostInfo::new("SuperDuper Test", "SuperDuperAI", "https://superduperai.co", "0.0.1").unwrap();
    let mut plugin = PluginInstance::<TH>::new(|_| TS, |_| (), &entry, c"co.superduperai.wind", &host_info)
        .expect("instantiate");

    let cfg = PluginAudioConfiguration {
        sample_rate: SR as f64,
        min_frames_count: BLOCK,
        max_frames_count: BLOCK,
    };
    let stopped = plugin.activate(|_, _| (), cfg).expect("activate");

    let total_frames = (SR * 27.0) as usize;
    let bu = BLOCK as usize;
    let n_blocks = total_frames / bu;

    // Build (sample, is_on, key, vel) event schedule.
    let mut evs: Vec<(usize, bool, u16, f64)> = Vec::new();
    for &(p, st, dur, v) in THEME {
        evs.push(((st * SR) as usize, true, p, v as f64 / 127.0));
        evs.push((((st + dur) * SR) as usize, false, p, 0.0));
    }

    let mut all_l = vec![0.0_f32; total_frames];
    let mut all_r = vec![0.0_f32; total_frames];
    let l_ref = &mut all_l;
    let r_ref = &mut all_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(2, 1);
            let mut out_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let start = block * bu;
                let end = start + bu;

                let mut in_buf = EventBuffer::new();
                // Block 0: if WIND_HOWL is set, dial in the howling-wind params
                // (Breath 0.95, Tone ~0, Howl 0.95, Gust 0.6) directly.
                if block == 0 && std::env::var("WIND_HOWL").is_ok() {
                    // Howling Gale showcase: Breath, low Tone, full Howl, strong Gust, Whistle
                    for (idx, val) in [(1u32, 0.95f64), (4, 0.05), (10, 1.0), (14, 0.8), (15, 0.9)] {
                        in_buf.push(&ParamValueEvent::new(
                            0,
                            ClapId::new(idx),
                            Pckn::new(0u16, 0u16, 0u16, 0u32),
                            val,
                            Cookie::empty(),
                        ));
                    }
                }
                for &(smp, on, key, vel) in &evs {
                    if smp >= start && smp < end {
                        let off = (smp - start) as u32;
                        if on {
                            in_buf.push(&NoteOnEvent::new(off, Pckn::new(0u16, 0u16, key, 0u32), vel));
                        } else {
                            in_buf.push(&NoteOffEvent::new(off, Pckn::new(0u16, 0u16, key, 0u32), 0.0));
                        }
                    }
                }
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);

                let mut in_l = vec![0.0_f32; bu];
                let mut in_r = vec![0.0_f32; bu];
                let mut in_chans: [&mut [f32]; 2] = [&mut in_l, &mut in_r];
                let mut out_l = vec![0.0_f32; bu];
                let mut out_r = vec![0.0_f32; bu];

                let input_audio = in_ports.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        in_chans.iter_mut().map(|b| InputChannel::variable(*b)),
                    ),
                }]);
                let mut out_chans: [&mut [f32]; 2] = [out_l.as_mut_slice(), out_r.as_mut_slice()];
                let mut output_audio = out_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(out_chans.iter_mut().map(|b| &mut **b)),
                }]);

                proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None)
                    .expect("process");

                l_ref[start..end].copy_from_slice(&out_l);
                r_ref[start..end].copy_from_slice(&out_r);
            }
            proc.stop_processing()
        })
        .join()
        .expect("thread")
    });
    plugin.deactivate(stopped_back);

    // interleave + write f32le
    let mut inter = Vec::with_capacity(total_frames * 2);
    for i in 0..total_frames {
        inter.push(all_l[i]);
        inter.push(all_r[i]);
    }
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(inter.as_ptr() as *const u8, inter.len() * 4) };
    let mut f = std::fs::File::create("/tmp/wind_theme.f32").expect("create");
    f.write_all(bytes).expect("write");
    eprintln!("wrote /tmp/wind_theme.f32  {} frames @ {} Hz stereo", total_frames, SR);
}
