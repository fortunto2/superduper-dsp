//! End-to-end CLAP integration tests for SuperDuper Wind — boot through
//! clack-host (no REAPER), exercise both `Mode`s:
//!   - Instrument: NoteOn → non-silent polyphonic output.
//!   - Overlay: feed audio in with Mode=Overlay → output differs from the
//!     dry input (breath was added on top).
//!
//! Run: cargo test --release -p superduper-wind --test clap_e2e -- --nocapture

use clack_common::events::event_types::ParamValueEvent;
use clack_common::events::Pckn;
use clack_common::utils::{ClapId, Cookie};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::event_types::NoteOnEvent;
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_plugin::entry::SinglePluginEntry;

use superduper_wind::{SuperDuperWind, P_MODE};

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

fn rms(samples: &[f32]) -> f32 {
    (samples.iter().map(|&x| x * x).sum::<f32>() / samples.len() as f32).sqrt()
}
fn peak(samples: &[f32]) -> f32 {
    samples.iter().map(|x| x.abs()).fold(0.0f32, f32::max)
}

const SR: f32 = 48_000.0;
const BLOCK: u32 = 256;

fn make_plugin(tag: &std::ffi::CStr) -> PluginInstance<TH> {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperWind>>(tag)
        .expect("plugin entry should load");
    let host_info =
        HostInfo::new("SuperDuper Test", "SuperDuperAI", "https://superduperai.co", "0.0.1")
            .unwrap();
    PluginInstance::<TH>::new(
        |_| TS,
        |_| (),
        &entry,
        c"co.superduperai.wind",
        &host_info,
    )
    .expect("plugin should instantiate")
}

#[test]
fn instrument_mode_produces_audio_on_noteon() {
    let mut plugin = make_plugin(c"/in/process/test/superduper-wind-instrument");

    let cfg = PluginAudioConfiguration {
        sample_rate: SR as f64,
        min_frames_count: BLOCK,
        max_frames_count: BLOCK,
    };
    let stopped = plugin.activate(|_, _| (), cfg).expect("activate");

    const SECONDS: f32 = 1.5;
    let total_frames = (SR * SECONDS) as usize;
    let block_usize = BLOCK as usize;
    let n_blocks = total_frames / block_usize;

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
                let start = block * block_usize;
                let end = start + block_usize;

                let mut in_buf = EventBuffer::new();
                if block == 0 {
                    // NoteOn key=57 (A3 = 220 Hz), full velocity — default
                    // Mode is Instrument (0.0) so no param event needed.
                    in_buf.push(&NoteOnEvent::new(0, Pckn::new(0u16, 0u16, 57u16, 0u32), 1.0));
                }
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);

                // Silent input — Instrument mode should ignore it entirely.
                let mut in_l = vec![0.0_f32; block_usize];
                let mut in_r = vec![0.0_f32; block_usize];
                let mut in_chans: [&mut [f32]; 2] = [&mut in_l, &mut in_r];
                let mut out_l = vec![0.0_f32; block_usize];
                let mut out_r = vec![0.0_f32; block_usize];

                let input_audio = in_ports.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        in_chans.iter_mut().map(|b| InputChannel::variable(*b)),
                    ),
                }]);
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
        })
        .join()
        .expect("audio thread")
    });
    plugin.deactivate(stopped_back);

    // Sustained section — skip the (soft, up to 3 s) attack ramp.
    let skip = (SR * 0.5) as usize;
    let tail_l = &all_l[skip..];
    let rms_l = rms(tail_l);
    let peak_l = peak(tail_l);
    eprintln!("Wind Instrument A3 (220 Hz): rms={rms_l:.5} peak={peak_l:.5}");
    assert!(all_l.iter().all(|v| v.is_finite()), "output has NaN/Inf");
    assert!(rms_l > 0.001, "Wind produced no audible audio through the CLAP pipeline (rms={rms_l})");
    assert!(peak_l < 0.99, "Wind is clipping (peak={peak_l})");
}

#[test]
fn overlay_mode_adds_breath_on_top_of_input() {
    let mut plugin = make_plugin(c"/in/process/test/superduper-wind-overlay");

    let cfg = PluginAudioConfiguration {
        sample_rate: SR as f64,
        min_frames_count: BLOCK,
        max_frames_count: BLOCK,
    };
    let stopped = plugin.activate(|_, _| (), cfg).expect("activate");

    const SECONDS: f32 = 1.5;
    let total_frames = (SR * SECONDS) as usize;
    let block_usize = BLOCK as usize;
    let n_blocks = total_frames / block_usize;

    // A steady 220 Hz tone as the "existing lead" Overlay should add
    // breath on top of.
    let mut src_l = vec![0.0_f32; total_frames];
    for (i, s) in src_l.iter_mut().enumerate() {
        let t = i as f32 / SR;
        *s = 0.3 * (std::f32::consts::TAU * 220.0 * t).sin();
    }
    let src_r = src_l.clone();

    let mut all_out_l = vec![0.0_f32; total_frames];
    let out_ref = &mut all_out_l;
    let src_l_ref = &src_l;
    let src_r_ref = &src_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(2, 1);
            let mut out_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let start = block * block_usize;
                let end = start + block_usize;

                let mut in_buf = EventBuffer::new();
                if block == 0 {
                    // Mode = Overlay (1.0), Breath/Mix already default to
                    // audible (0.5 each from the boot-time Kurai preset).
                    in_buf.push(&ParamValueEvent::new(
                        0,
                        ClapId::new(P_MODE as u32),
                        Pckn::new(0u16, 0u16, 0u16, 0u32),
                        1.0,
                        Cookie::empty(),
                    ));
                }
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);

                let mut in_l = src_l_ref[start..end].to_vec();
                let mut in_r = src_r_ref[start..end].to_vec();
                let mut in_chans: [&mut [f32]; 2] = [&mut in_l, &mut in_r];
                let mut out_l = vec![0.0_f32; block_usize];
                let mut out_r = vec![0.0_f32; block_usize];

                let input_audio = in_ports.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        in_chans.iter_mut().map(|b| InputChannel::variable(*b)),
                    ),
                }]);
                let mut out_chans: [&mut [f32]; 2] = [out_l.as_mut_slice(), out_r.as_mut_slice()];
                let mut output_audio = out_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        out_chans.iter_mut().map(|b| &mut **b),
                    ),
                }]);

                proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None)
                    .expect("process");

                out_ref[start..end].copy_from_slice(&out_l);
            }
            proc.stop_processing()
        })
        .join()
        .expect("audio thread")
    });
    plugin.deactivate(stopped_back);

    assert!(all_out_l.iter().all(|v| v.is_finite()), "Overlay output has NaN/Inf");

    // Past the envelope-follower warm-up, compare against the dry source —
    // Overlay must have added something (the breath layer), not just
    // passed audio through unchanged.
    let tail = SR as usize; // last second
    let diff_energy: f32 = (all_out_l.len() - tail..all_out_l.len())
        .map(|i| {
            let d = all_out_l[i] - src_l[i];
            d * d
        })
        .sum::<f32>()
        / tail as f32;
    let diff_rms = diff_energy.sqrt();
    let src_rms: f32 = (all_out_l.len() - tail..all_out_l.len())
        .map(|i| src_l[i] * src_l[i])
        .sum::<f32>()
        .sqrt()
        / (tail as f32).sqrt();
    eprintln!(
        "Overlay mode: output-vs-input diff rms = {diff_rms:.5} (source rms = {src_rms:.5}, \
         {:.1}% of source level — must be OBVIOUS, not a subtle tweak)",
        100.0 * diff_rms / src_rms
    );
    // The feedback that produced this test: "Overlay barely changes the
    // sound... at Mix=0.5 the effect must be obvious." A relative
    // threshold (not an absolute 1e-4) actually enforces that — the wind
    // bed + gust-driven ducking filter must move the signal by a real
    // fraction of its own level, not a barely-measurable epsilon.
    assert!(
        diff_rms > src_rms * 0.1,
        "Overlay must audibly transform the input (wind bed + ducking), not just nudge it: \
         diff_rms={diff_rms:.5} is only {:.1}% of source_rms={src_rms:.5}",
        100.0 * diff_rms / src_rms
    );
}
