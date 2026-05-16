//! End-to-end CLAP test — driving SuperDuper Supermass through the full
//! plugin pipeline (entry → factory → instance → activate → process)
//! using clack-host as the host. Confirms the CLAP wiring is correct,
//! independent of REAPER.
//!
//! Run with: `cargo test -p superduper-supermass --test clap_e2e -- --nocapture`

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_plugin::entry::SinglePluginEntry;

use superduper_supermass::SuperDuperSupermass;

struct HS;
impl SharedHandler<'_> for HS {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}
impl HostLogImpl for HS {
    fn log(&self, severity: LogSeverity, message: &str) {
        eprintln!("[plugin {severity}] {message}");
    }
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
fn supermass_clap_pipeline_works() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperSupermass>>(
        c"/in/process/test/supermass",
    )
    .expect("entry loads");
    let factory = entry
        .get_factory::<clack_host::factory::plugin::PluginFactory>()
        .expect("factory");
    let desc = factory.plugin_descriptor(0).unwrap();
    assert_eq!(desc.id().unwrap().to_bytes(), b"co.superduperai.supermass");

    let host_info =
        HostInfo::new("Test", "SuperDuperAI", "https://superduperai.co", "0.1").unwrap();
    let mut inst = PluginInstance::<H>::new(
        |_| HS,
        |_| (),
        &entry,
        c"co.superduperai.supermass",
        &host_info,
    )
    .expect("instantiate");

    const SR: f32 = 48_000.0;
    const BLOCK: u32 = 512;
    let cfg = PluginAudioConfiguration {
        sample_rate: SR as f64,
        min_frames_count: BLOCK,
        max_frames_count: BLOCK,
    };
    let stopped = inst.activate(|_, _| (), cfg).expect("activate");

    // 1 second of impulse-then-silence to confirm dry+wet mix produces a
    // signal different from pure passthrough.
    const TOTAL: usize = SR as usize;
    const BU: usize = BLOCK as usize;
    let n_blocks = TOTAL / BU;
    let mut in_l = vec![0.0_f32; TOTAL];
    let mut in_r = vec![0.0_f32; TOTAL];
    for s in in_l.iter_mut().take(8) { *s = 1.0; }
    for s in in_r.iter_mut().take(8) { *s = 1.0; }
    let mut out_l = vec![0.0_f32; TOTAL];
    let mut out_r = vec![0.0_f32; TOTAL];

    let il = &mut in_l;
    let ir = &mut in_r;
    let ol = &mut out_l;
    let or_ = &mut out_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut ap = stopped.start_processing().expect("start_processing");
            let mut input_ports = AudioPorts::with_capacity(2, 1);
            let mut output_ports = AudioPorts::with_capacity(2, 1);

            for b in 0..n_blocks {
                let st = b * BU;
                let en = st + BU;
                let il_chunk = &mut il[st..en];
                let ir_chunk = &mut ir[st..en];
                let mut in_chans: [&mut [f32]; 2] = [il_chunk, ir_chunk];
                let mut ol_chunk = vec![0.0_f32; BU];
                let mut or_chunk = vec![0.0_f32; BU];

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

                ap.process(&input_audio, &mut output_audio, &input_events, &mut output_events, None, None)
                    .expect("process");

                ol[st..en].copy_from_slice(&ol_chunk);
                or_[st..en].copy_from_slice(&or_chunk);
            }
            ap.stop_processing()
        })
        .join()
        .expect("audio thread")
    });

    inst.deactivate(stopped_back);

    // Mix dethemes at default 0.3; output must differ from input.
    let mut diff_sq: f32 = 0.0;
    for i in 0..TOTAL {
        let d = out_l[i] - in_l[i];
        diff_sq += d * d;
    }
    let diff_rms = (diff_sq / TOTAL as f32).sqrt();
    eprintln!("supermass clap_e2e: diff_rms = {diff_rms:.6}");
    assert!(diff_rms > 1e-4, "output identical to input (diff_rms={diff_rms}); CLAP pipeline broken");
}
