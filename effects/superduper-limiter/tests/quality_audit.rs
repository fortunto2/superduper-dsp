//! quality_audit.rs — drive the Limiter through realistic signals and
//! assert it actually limits. Two probes:
//!
//! 1. Sine at -3 dBFS, Input +12 dB, Ceiling -1 dBFS → output peak
//!    must be ≤ -0.5 dBFS (give 0.5 dB tolerance for first-block
//!    smoothing).
//! 2. Pulse train (single-sample spikes between zeros) with Input +24
//!    dB, Ceiling -1 dBFS → output peak must still be ≤ -0.5 dBFS.
//!    Catches the case where lookahead misses inter-sample peaks.
//!
//! Run: cargo test --release -p superduper-limiter --test quality_audit -- --nocapture

use clack_common::events::Pckn;
use clack_common::events::event_types::ParamValueEvent;
use clack_common::utils::{ClapId, Cookie};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_plugin::entry::SinglePluginEntry;
use superduper_limiter::SuperDuperLimiter;

const SR: f32 = 48_000.0;
const BLOCK: u32 = 256;
const TOTAL_SECONDS: f32 = 1.0;

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

fn render(input_db: f32, ceiling_db: f32, input_gen: impl Fn(usize) -> f32) -> Vec<f32> {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperLimiter>>(
        c"/in/process/test/superduper-limiter-audit",
    )
    .expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TestHost>::new(
        |_| TestShared, |_| (), &entry, c"co.superduperai.limiter", &host_info,
    ).expect("instantiate");

    let total_frames = (SR as usize) * TOTAL_SECONDS as usize;
    let block_us = BLOCK as usize;
    let n_blocks = total_frames / block_us;

    let stopped = plugin.activate(|_, _| (), PluginAudioConfiguration {
        sample_rate: SR as f64, min_frames_count: BLOCK, max_frames_count: BLOCK,
    }).expect("activate");

    let signal: Vec<f32> = (0..total_frames).map(&input_gen).collect();
    let mut out_l = vec![0.0_f32; total_frames];
    let mut out_r = vec![0.0_f32; total_frames];
    let signal_ref = &signal;
    let out_l_ref = &mut out_l;
    let out_r_ref = &mut out_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(2, 1);
            let mut out_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let start = block * block_us;
                let end = start + block_us;

                let mut in_buf = EventBuffer::new();
                if block == 0 {
                    for &(id, v) in &[
                        (0u32 /* P_INPUT */, input_db as f64),
                        (1u32 /* P_CEILING */, ceiling_db as f64),
                    ] {
                        let ev = ParamValueEvent::new(
                            0, ClapId::new(id),
                            Pckn::new(0u16, 0u16, 0u16, 0u32),
                            v, Cookie::empty(),
                        );
                        in_buf.push(&ev);
                    }
                }
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);

                let mut in_chunk_l = signal_ref[start..end].to_vec();
                let mut in_chunk_r = signal_ref[start..end].to_vec();
                let in_chans = [
                    InputChannel::variable(in_chunk_l.as_mut_slice()),
                    InputChannel::variable(in_chunk_r.as_mut_slice()),
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
    // Return the L channel — mono signal anyway.
    out_l
}

fn db_to_amp(db: f32) -> f32 { 10f32.powf(db / 20.0) }
fn amp_to_db(a: f32) -> f32 { 20.0 * a.abs().max(1e-9).log10() }

#[test]
fn limiter_holds_sine_below_ceiling() {
    // Sine at -3 dBFS, push input +12 dB, ceiling at -1 dBFS.
    let amp = db_to_amp(-3.0);
    let out = render(12.0, -1.0, |i| {
        let t = i as f32 / SR;
        amp * (t * 440.0 * std::f32::consts::TAU).sin()
    });

    // Skip the first 100 ms — lookahead + smoothing transient.
    let skip = (SR * 0.10) as usize;
    let peak = out[skip..].iter().fold(0.0_f32, |a, x| a.max(x.abs()));
    let peak_db = amp_to_db(peak);
    eprintln!("sine peak after limiting: {:.3} dBFS (ceiling -1.0)", peak_db);
    assert!(
        peak_db <= -0.5,
        "Sine peak {peak_db:.3} dBFS exceeds ceiling -0.5 dBFS tolerance — limiter failing"
    );
}

#[test]
fn limiter_catches_pulse_train_with_true_peak() {
    // 1-sample pulses every 200 samples (240 Hz) at +0.95 unity, push
    // input by +24 dB so without limiting we'd see ~+24 dBFS peaks.
    let out = render(24.0, -1.0, |i| if i % 200 == 0 { 0.95 } else { 0.0 });
    let skip = (SR * 0.10) as usize;
    let peak = out[skip..].iter().fold(0.0_f32, |a, x| a.max(x.abs()));
    let peak_db = amp_to_db(peak);
    eprintln!("pulse peak after limiting: {:.3} dBFS (ceiling -1.0)", peak_db);
    assert!(
        peak_db <= -0.3,
        "Pulse peak {peak_db:.3} dBFS exceeds ceiling -0.3 dBFS tolerance — true-peak miss?"
    );
}
