//! Drum smoke: send a few MIDI hits (C1 kick, D1 snare, F#1 hat) and
//! verify (a) audio comes out, (b) a passthrough note (C3) reaches
//! the output event queue.

use clack_common::events::Pckn;
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::event_types::{NoteOnEvent, NoteOffEvent};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, AudioPorts};
use clack_plugin::entry::SinglePluginEntry;
use superduper_drum::SuperDuperDrum;

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
    fn declare_extensions(b: &mut HostExtensions<Self>, _: &Self::Shared<'_>) {
        b.register::<HostLog>();
    }
}

#[test]
fn drum_triggers_and_passes_through_bass_notes() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperDrum>>(
        c"/in/process/test/superduper-drum-smoke",
    ).expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TH>::new(
        |_| TS, |_| (), &entry, c"co.superduperai.drum", &host_info,
    ).expect("instantiate");

    const SR: f32 = 48_000.0;
    const BLOCK: u32 = 256;
    let total: usize = (SR * 1.5) as usize;
    let n_blocks = total / BLOCK as usize;
    let stopped = plugin.activate(|_, _| (), PluginAudioConfiguration {
        sample_rate: SR as f64, min_frames_count: BLOCK, max_frames_count: BLOCK,
    }).expect("activate");

    let mut out_l = vec![0.0_f32; total];
    let mut out_r = vec![0.0_f32; total];
    let l_ref = &mut out_l;
    let r_ref = &mut out_r;
    let passthrough_count = std::sync::Mutex::new(0_usize);
    let passthrough_ref = &passthrough_count;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(0, 0);
            let mut out_ports = AudioPorts::with_capacity(2, 1);
            for block in 0..n_blocks {
                let start = block * BLOCK as usize;
                let end = start + BLOCK as usize;
                let mut in_buf = EventBuffer::new();
                // Block 0: kick (C1 = 36), snare (D1 = 38), hat (F#1 = 42),
                // and a bass note E3 = 52 — E is NOT in the drum map
                // (those are C/D/D#/F#/G#/A#), so it should pass through.
                if block == 0 {
                    for key in [36u16, 38, 42, 52] {
                        let on = NoteOnEvent::new(0, Pckn::new(0u16, 0u16, key, 0u32), 0.9);
                        in_buf.push(&on);
                    }
                }
                // Block 4: release the bass note.
                if block == 4 {
                    let off = NoteOffEvent::new(0, Pckn::new(0u16, 0u16, 52u16, 0u32), 1.0);
                    in_buf.push(&off);
                }
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);
                let mut out_l_chunk = vec![0.0_f32; BLOCK as usize];
                let mut out_r_chunk = vec![0.0_f32; BLOCK as usize];
                let input_audio = in_ports.with_input_buffers(std::iter::empty::<
                    AudioPortBuffer<
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f32>>,
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f64>>,
                    >,
                >());
                let mut out_chans: [&mut [f32]; 2] =
                    [out_l_chunk.as_mut_slice(), out_r_chunk.as_mut_slice()];
                let mut output_audio = out_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        out_chans.iter_mut().map(|b| &mut **b),
                    ),
                }]);
                proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None)
                    .expect("process");
                // Count passthrough notes in the output queue.
                for ev in out_evs.iter() {
                    if ev.as_event::<NoteOnEvent>().is_some()
                        || ev.as_event::<NoteOffEvent>().is_some()
                    {
                        *passthrough_ref.lock().unwrap() += 1;
                    }
                }
                l_ref[start..end].copy_from_slice(&out_l_chunk);
                r_ref[start..end].copy_from_slice(&out_r_chunk);
            }
            proc.stop_processing()
        }).join().expect("audio")
    });
    plugin.deactivate(stopped_back);

    let rms_l: f32 = (out_l.iter().map(|x| x * x).sum::<f32>() / out_l.len() as f32).sqrt();
    let peak = out_l.iter().fold(0.0_f32, |a, x| a.max(x.abs()));
    let count = *passthrough_count.lock().unwrap();
    eprintln!("Drum: rms_l={:.4}, peak={:.4}, passthrough events={}", rms_l, peak, count);
    assert!(rms_l > 0.005, "drum output silent (rms={rms_l})");
    assert!(peak < 0.99, "drum clipping (peak={peak})");
    for x in &out_l { assert!(x.is_finite(), "NaN/Inf in drum output"); }
    // Expect at least 2 passthrough events (NoteOn C3 + NoteOff C3).
    assert!(count >= 2, "Passthrough didn't fire — note routing broken (count={count})");
}
