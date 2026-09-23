//! Two-port CLAP e2e: does the compressor actually key off the SIDECHAIN
//! port, latch it, and release when the routed key goes silent?
//!
//! This is the AI-TODO that sat in lib.rs since the 2026-08-27 latch fix —
//! until now the sidechain path was verified only through sdsp-chain
//! renders. It also settles a REAPER debugging session that concluded "the
//! key never reaches the plugin" from A/B renders of NON-deterministic
//! synths, where a Range-limited 3 dB duck cannot be seen under a 9.8 dB
//! render-to-render noise floor. Here everything is deterministic.
//!
//! Scenario (6 s): main = 200 Hz at −20 dBFS throughout. Key on port 1:
//! silent → 2 s, −6 dBFS 100 Hz burst 2–4 s, silent again 4–6 s.
//!
//!   phase 1  key silent, latch open  → detector falls back to DRY:
//!            −20 over a −30 threshold at 10:1 → GR ≈ −9 → out ≈ −29 dBFS
//!   phase 2  key sounding, latch set → detector = KEY: −6 over −30
//!            → GR ≈ −21.6 → out ≈ −41.6 dBFS  (key audio itself must NOT
//!            leak into the output — only the gain moves)
//!   phase 3  key silent, latch HELD  → detector sees the silent key,
//!            GR releases → out ≈ −20 dBFS (louder than phase 1: the exact
//!            behaviour the 08-27 latch fix promised)
//!
//! A second run with Range = 5 must clamp phase 2 to ≈ −25 dBFS.

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
use superduper_compressor::SuperDuperCompressor;

const SR: f32 = 48_000.0;
const BLOCK: u32 = 256;
const TOTAL_SECONDS: usize = 6;

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

fn db(rms: f32) -> f32 {
    20.0 * rms.max(1e-9).log10()
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

/// Render the scenario, return output RMS in dBFS for the three phases.
fn render(range_db: f32) -> (f32, f32, f32) {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperCompressor>>(
        c"/in/process/test/superduper-compressor-sc",
    )
    .expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TestHost>::new(
        |_| TestShared,
        |_| (),
        &entry,
        c"co.superduperai.compressor",
        &host_info,
    )
    .expect("instantiate");

    let total_frames = SR as usize * TOTAL_SECONDS;
    let block_us = BLOCK as usize;
    let n_blocks = total_frames / block_us;

    let stopped = plugin
        .activate(
            |_, _| (),
            PluginAudioConfiguration {
                sample_rate: SR as f64,
                min_frames_count: BLOCK,
                max_frames_count: BLOCK,
            },
        )
        .expect("activate");

    // Main: −20 dBFS 200 Hz all the way. Key: −6 dBFS 100 Hz in [2 s, 4 s).
    let main: Vec<f32> = (0..total_frames)
        .map(|i| 0.1 * (core::f32::consts::TAU * 200.0 * i as f32 / SR).sin())
        .collect();
    let key: Vec<f32> = (0..total_frames)
        .map(|i| {
            let t = i as f32 / SR;
            if (2.0..4.0).contains(&t) {
                0.5 * (core::f32::consts::TAU * 100.0 * i as f32 / SR).sin()
            } else {
                0.0
            }
        })
        .collect();

    let mut out_l = vec![0.0_f32; total_frames];
    let (main_ref, key_ref, out_ref) = (&main, &key, &mut out_l);

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(2, 2);
            let mut out_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let start = block * block_us;
                let end = start + block_us;

                let mut in_buf = EventBuffer::new();
                if block == 0 {
                    // (id, value): threshold −30, ratio 10, attack 2 ms,
                    // release 150 ms, makeup 0, SC HPF at the 20 Hz floor,
                    // knee 0 (exact arithmetic), curve Clean, and Range under
                    // test. Real units, not 0..1.
                    for &(id, v) in &[
                        (0u32, -30.0f64),
                        (1, 10.0),
                        (2, 2.0),
                        (3, 150.0),
                        (5, 0.0),
                        (6, 20.0),
                        (4, 0.0),
                        (13, 0.0),
                        (14, range_db as f64),
                    ] {
                        in_buf.push(&ParamValueEvent::new(
                            0,
                            ClapId::new(id),
                            Pckn::new(0u16, 0u16, 0u16, 0u32),
                            v,
                            Cookie::empty(),
                        ));
                    }
                }
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);

                let mut main_l = main_ref[start..end].to_vec();
                let mut main_r = main_ref[start..end].to_vec();
                let mut key_l = key_ref[start..end].to_vec();
                let mut key_r = key_ref[start..end].to_vec();
                // One OUTER map over port-pairs: a single closure site makes
                // every AudioPortBuffer the same anonymous type, which
                // with_input_buffers needs (it takes a homogeneous iterator).
                let mut port_ch: [[&mut [f32]; 2]; 2] = [
                    [&mut main_l, &mut main_r],
                    [&mut key_l, &mut key_r],
                ];
                let input_audio =
                    in_ports.with_input_buffers(port_ch.iter_mut().map(|pair| AudioPortBuffer {
                        latency: 0,
                        channels: AudioPortBufferType::f32_input_only(
                            pair.iter_mut().map(|b| InputChannel::variable(*b)),
                        ),
                    }));

                let mut o_l = vec![0.0_f32; block_us];
                let mut o_r = vec![0.0_f32; block_us];
                let mut out_ch: [&mut [f32]; 2] = [&mut o_l, &mut o_r];
                let mut output_audio = out_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        out_ch.iter_mut().map(|b| &mut **b),
                    ),
                }]);

                proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None)
                    .expect("process");
                out_ref[start..end].copy_from_slice(&o_l);
            }
            proc.stop_processing()
        })
        .join()
        .expect("audio thread")
    });
    plugin.deactivate(stopped_back);

    let sec = SR as usize;
    let p1 = db(rms(&out_l[sec..sec * 19 / 10]));
    let p2 = db(rms(&out_l[sec * 5 / 2..sec * 39 / 10]));
    let p3 = db(rms(&out_l[sec * 11 / 2..sec * 59 / 10]));
    eprintln!("range={range_db}: p1={p1:.1} p2={p2:.1} p3={p3:.1} dBFS");
    (p1, p2, p3)
}

#[test]
fn sidechain_keys_latches_and_releases() {
    let (p1, p2, p3) = render(0.0);
    // All expectations are OUTPUT RMS of a sine: amplitude-dB minus 3.01.
    // Phase 1: no key yet → dry fallback compresses −20 by ~9 dB → −32 RMS.
    assert!((p1 - -32.0).abs() < 1.5, "dry-fallback phase off: {p1:.1} (want ≈ −32)");
    // Phase 2: the burst on PORT 1 must deepen GR to ~21.6 dB — this is the
    // sidechain actually keying (measured −44.5 on first run: within 0.1 dB
    // of theory). The key tone itself must not leak into the output.
    assert!((p2 - -44.6).abs() < 1.5, "keyed phase off: {p2:.1} (want ≈ −44.6)");
    // Phase 3: key routed-then-silent → GR must RELEASE (the latch holds the
    // key SELECTION, not the compression). Window sits 1.5 s after the burst
    // so the deep-GR release tail has fully run out.
    assert!((p3 - -23.0).abs() < 2.0, "release-after-silent-key off: {p3:.1} (want ≈ −23)");
    assert!(p3 > p1 + 5.0 && p1 > p2 + 5.0, "phase ordering broken");
}

#[test]
fn range_clamps_the_sidechain_path_too() {
    let (_, p2, _) = render(5.0);
    // Same keyed phase, Range 5 → GR capped at 5 → out ≈ −28 RMS
    // (measured −28.0 on first run — exactly the clamp).
    assert!((p2 - -28.0).abs() < 1.5, "Range on the SC path off: {p2:.1} (want ≈ −28)");
}
