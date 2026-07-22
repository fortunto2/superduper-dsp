//! End-to-end CLAP integration test for SuperDuper Tune.
//!
//! Run: `cargo test -p superduper-tune --test clap_e2e -- --nocapture`

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_plugin::entry::SinglePluginEntry;

use superduper_tune::SuperDuperTune;

struct TestHostShared;
impl SharedHandler<'_> for TestHostShared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}
impl HostLogImpl for TestHostShared {
    fn log(&self, severity: LogSeverity, message: &str) {
        eprintln!("[plugin {severity}] {message}");
    }
}
struct TestHost;
impl HostHandlers for TestHost {
    type Shared<'a> = TestHostShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
    fn declare_extensions(builder: &mut HostExtensions<Self>, _: &Self::Shared<'_>) {
        builder.register::<HostLog>();
    }
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|&v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}
fn peak(x: &[f32]) -> f32 {
    x.iter().map(|v| v.abs()).fold(0.0f32, f32::max)
}

#[test]
fn tune_clap_pipeline_produces_audio() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperTune>>(
        c"/in/process/test/superduper-tune",
    )
    .expect("plugin entry should load");
    let factory = entry
        .get_factory::<clack_host::factory::plugin::PluginFactory>()
        .expect("plugin factory must be present");
    let desc = factory.plugin_descriptor(0).unwrap();
    assert_eq!(desc.id().unwrap().to_bytes(), b"co.superduperai.tune");

    let host_info =
        HostInfo::new("SuperDuper Test", "SuperDuperAI", "https://superduperai.co", "0.0.1")
            .unwrap();
    let mut plugin_instance = PluginInstance::<TestHost>::new(
        |_| TestHostShared,
        |_| (),
        &entry,
        c"co.superduperai.tune",
        &host_info,
    )
    .expect("plugin should instantiate");

    const SR: f32 = 48_000.0;
    const BLOCK: u32 = 512;
    let cfg = PluginAudioConfiguration {
        sample_rate: SR as f64,
        min_frames_count: BLOCK,
        max_frames_count: BLOCK,
    };
    let stopped = plugin_instance.activate(|_, _| (), cfg).expect("activate");

    const TOTAL: usize = (SR as usize) * 2;
    const BU: usize = BLOCK as usize;
    let n_blocks = TOTAL / BU;

    // A slightly-sharp A (460 Hz) so the default Major-scale correction has work.
    let mut in_l = vec![0.0f32; TOTAL];
    let mut in_r = vec![0.0f32; TOTAL];
    for i in 0..TOTAL {
        let t = i as f32 / SR;
        let s = (std::f32::consts::TAU * 460.0 * t).sin() * 0.3;
        in_l[i] = s;
        in_r[i] = s;
    }
    let mut out_l = vec![0.0f32; TOTAL];
    let mut out_r = vec![0.0f32; TOTAL];

    let ol = &mut out_l;
    let or = &mut out_r;
    let il = &mut in_l;
    let ir = &mut in_r;
    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut ap = stopped.start_processing().expect("start_processing");
            let mut inp = AudioPorts::with_capacity(2, 1);
            let mut outp = AudioPorts::with_capacity(2, 1);
            for b in 0..n_blocks {
                let st = b * BU;
                let en = st + BU;
                let lc = &mut il[st..en];
                let rc = &mut ir[st..en];
                let mut ins: [&mut [f32]; 2] = [lc, rc];
                let mut olc = vec![0.0f32; BU];
                let mut orc = vec![0.0f32; BU];
                let ev: [clack_host::events::event_types::NoteOnEvent; 0] = [];
                let iev = InputEvents::from_buffer(&ev);
                let mut oevb = EventBuffer::new();
                let mut oev = OutputEvents::from_buffer(&mut oevb);
                let ia = inp.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        ins.iter_mut().map(|x| InputChannel::variable(*x)),
                    ),
                }]);
                let mut ocs: [&mut [f32]; 2] = [olc.as_mut_slice(), orc.as_mut_slice()];
                let mut oa = outp.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(ocs.iter_mut().map(|x| &mut **x)),
                }]);
                ap.process(&ia, &mut oa, &iev, &mut oev, None, None).expect("process");
                ol[st..en].copy_from_slice(&olc);
                or[st..en].copy_from_slice(&orc);
            }
            ap.stop_processing()
        })
        .join()
        .expect("audio thread")
    });
    plugin_instance.deactivate(stopped_back);

    let tail = &out_l[TOTAL / 2..];
    let (pk, r) = (peak(tail), rms(tail));
    eprintln!("CLAP e2e: tail peak={pk:.5} rms={r:.5}");
    assert!(out_l.iter().all(|v| v.is_finite()), "non-finite output");
    assert!(pk > 1e-3, "tune plugin silent through CLAP pipeline (peak={pk})");
}
