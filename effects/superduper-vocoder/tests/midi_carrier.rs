//! End-to-end MIDI-carrier test through the full CLAP pipeline.
//!
//! Proves the note-ports plumbing works: with `Pitch Source = MIDI` and no
//! unvoiced noise, the carrier (and therefore the output) is SILENT until a
//! MIDI NoteOn arrives, then sounds. Catches broken note-ports declaration /
//! event routing that the DSP-level tests can't see.
//!
//! Run: `cargo test -p superduper-vocoder --test midi_carrier -- --nocapture`

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

use superduper_vocoder::{SuperDuperVocoder, P_PITCH_SOURCE, P_UNVOICED};

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
    (x.iter().map(|&v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[test]
fn midi_note_drives_carrier_through_clap() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperVocoder>>(
        c"/in/process/test/superduper-vocoder-midi",
    )
    .expect("plugin entry should load");
    let host_info =
        HostInfo::new("SuperDuper Test", "SuperDuperAI", "https://superduperai.co", "0.0.1")
            .unwrap();
    let mut plugin_instance = PluginInstance::<TestHost>::new(
        |_| TestHostShared,
        |_| (),
        &entry,
        c"co.superduperai.vocoder",
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

    const BLOCK_USIZE: usize = BLOCK as usize;
    let n_blocks = 120; // ~1.3 s
    let note_block = n_blocks / 2;

    // Voiced modulator (220 Hz + harmonics) on the audio input, every block.
    let make_mod = |start: usize| -> Vec<f32> {
        (0..BLOCK_USIZE)
            .map(|k| {
                let t = (start + k) as f32 / SR;
                let mut s = 0.0;
                for h in 1..=6 {
                    s += (1.0 / h as f32) * (std::f32::consts::TAU * 220.0 * h as f32 * t).sin();
                }
                s * 0.2
            })
            .collect()
    };

    let (stopped_back, silent_rms, note_rms) = std::thread::scope(|s| {
        s.spawn(move || {
            let mut silent_rms = 0.0f32;
            let mut note_rms = 0.0f32;
            let mut audio_proc = stopped.start_processing().expect("start_processing");
            let mut input_ports = AudioPorts::with_capacity(2, 1);
            let mut output_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let mut in_l = make_mod(block * BLOCK_USIZE);
                let mut in_r = in_l.clone();
                let mut in_chans: [&mut [f32]; 2] = [&mut in_l, &mut in_r];

                let mut out_l = vec![0.0f32; BLOCK_USIZE];
                let mut out_r = vec![0.0f32; BLOCK_USIZE];

                let mut in_buf = EventBuffer::new();
                if block == 0 {
                    // Pitch Source = MIDI (1.0), Unvoiced = 0 → carrier silent
                    // without keys.
                    in_buf.push(&ParamValueEvent::new(
                        0,
                        ClapId::new(P_PITCH_SOURCE as u32),
                        Pckn::new(0u16, 0u16, 0u16, 0u32),
                        1.0,
                        Cookie::empty(),
                    ));
                    in_buf.push(&ParamValueEvent::new(
                        0,
                        ClapId::new(P_UNVOICED as u32),
                        Pckn::new(0u16, 0u16, 0u16, 0u32),
                        0.0,
                        Cookie::empty(),
                    ));
                }
                if block == note_block {
                    // NoteOn C4 (60).
                    in_buf.push(&NoteOnEvent::new(0, Pckn::new(0u16, 0u16, 60u16, 0u32), 1.0));
                }
                let input_events = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut output_events = OutputEvents::from_buffer(&mut out_evs);

                let input_audio = input_ports.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        in_chans.iter_mut().map(|b| InputChannel::variable(*b)),
                    ),
                }]);
                let mut out_chans: [&mut [f32]; 2] = [&mut out_l, &mut out_r];
                let mut output_audio = output_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        out_chans.iter_mut().map(|b| &mut **b),
                    ),
                }]);

                audio_proc
                    .process(&input_audio, &mut output_audio, &input_events, &mut output_events, None, None)
                    .expect("process");

                assert!(out_l.iter().all(|v| v.is_finite()), "NaN/Inf in block {block}");
                let r = rms(&out_l);
                if block == note_block - 4 {
                    silent_rms = r;
                }
                if block > note_block + 5 && r > note_rms {
                    note_rms = r; // peak rms after the note settles
                }
                let _ = r;
            }
            (audio_proc.stop_processing(), silent_rms, note_rms)
        })
        .join()
        .expect("audio thread")
    });
    plugin_instance.deactivate(stopped_back);

    eprintln!("MIDI carrier: no-keys rms={silent_rms:.6}  note-held rms={note_rms:.6}");
    assert!(
        silent_rms < 1e-3,
        "MIDI mode with no keys + no unvoiced should be silent (rms={silent_rms})"
    );
    assert!(
        note_rms > 1e-2,
        "held MIDI note should drive the carrier (rms={note_rms})"
    );
}
