//! choke_audit.rs — proves the REAPER "click on timeline relocate" symptom.
//!
//! Scenario: hold a note long enough to reach sustain, then send a
//! NoteChoke. The current implementation hard-cuts the envelope, which
//! produces a sample-to-sample discontinuity equal to the sustain level
//! — audible as a click. A correct implementation fades out within a few
//! ms so |Δx[n]| stays in the bandlimited range.

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::Pckn;
use clack_host::events::event_types::{NoteOnEvent};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, AudioPorts};
use clack_plugin::entry::SinglePluginEntry;
use superduper_pad::SuperDuperPad;

// NoteChoke event constructor — same shape as NoteOn/NoteOff with kind=Choke.
// clack-host exposes it through event_types::NoteChokeEvent.
use clack_host::events::event_types::NoteChokeEvent;

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

const SR: f32 = 48_000.0;
const BLOCK: u32 = 256;

#[test]
fn note_choke_does_not_produce_audible_click() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperPad>>(
        c"/in/process/test/superduper-pad-choke-audit",
    )
    .expect("plugin entry should load");

    let host_info = HostInfo::new("SDSP Test", "SuperDuperAI", "https://superduperai.co", "0").unwrap();
    let mut plugin = PluginInstance::<TestHost>::new(
        |_| TestHostShared,
        |_| (),
        &entry,
        c"co.superduperai.pad",
        &host_info,
    )
    .expect("instantiate");

    let cfg = PluginAudioConfiguration {
        sample_rate: SR as f64,
        min_frames_count: BLOCK,
        max_frames_count: BLOCK,
    };
    let stopped = plugin.activate(|_, _| (), cfg).expect("activate");

    const BLOCK_USIZE: usize = BLOCK as usize;
    // 1.5 s total — 1 s for note-on to reach sustain, then choke at sample
    // 48000 (t=1.0s) and observe the following 0.5 s.
    let total_frames: usize = (SR as usize) * 3 / 2;
    let n_blocks = total_frames / BLOCK_USIZE;
    let choke_sample: usize = SR as usize; // 1.0 s
    let key: u16 = 60;

    let mut all_l = vec![0.0_f32; total_frames];
    let mut all_r = vec![0.0_f32; total_frames];
    let l_ref = &mut all_l;
    let r_ref = &mut all_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut audio_proc = stopped.start_processing().expect("start_processing");
            let mut input_ports = AudioPorts::with_capacity(0, 0);
            let mut output_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let start = block * BLOCK_USIZE;
                let end = start + BLOCK_USIZE;

                let mut input_buf = EventBuffer::new();
                // NoteOn at sample 0 of block 0.
                if block == 0 {
                    let pckn = Pckn::new(0u16, 0u16, key, 0u32);
                    input_buf.push(&NoteOnEvent::new(0, pckn, 0.9));
                }
                // NoteChoke when sample crosses choke_sample.
                if choke_sample >= start && choke_sample < end {
                    let local = (choke_sample - start) as u32;
                    let pckn = Pckn::new(0u16, 0u16, key, 0u32);
                    input_buf.push(&NoteChokeEvent::new(local, pckn));
                }

                let input_events = InputEvents::from_buffer(&input_buf);
                let mut out_evs = EventBuffer::new();
                let mut output_events = OutputEvents::from_buffer(&mut out_evs);

                let mut out_l_chunk = vec![0.0_f32; BLOCK_USIZE];
                let mut out_r_chunk = vec![0.0_f32; BLOCK_USIZE];

                let input_audio = input_ports.with_input_buffers(std::iter::empty::<
                    AudioPortBuffer<
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f32>>,
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f64>>,
                    >,
                >());

                let mut out_chans: [&mut [f32]; 2] =
                    [out_l_chunk.as_mut_slice(), out_r_chunk.as_mut_slice()];
                let mut output_audio = output_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        out_chans.iter_mut().map(|b| &mut **b),
                    ),
                }]);

                audio_proc
                    .process(
                        &input_audio,
                        &mut output_audio,
                        &input_events,
                        &mut output_events,
                        None,
                        None,
                    )
                    .expect("process should succeed");

                l_ref[start..end].copy_from_slice(&out_l_chunk);
                r_ref[start..end].copy_from_slice(&out_r_chunk);
            }
            audio_proc.stop_processing()
        })
        .join()
        .expect("audio thread")
    });
    plugin.deactivate(stopped_back);

    // Examine sample-to-sample discontinuity in a small window around the
    // choke sample. Bandlimited audio at ≤2 kHz max partial × 0.7 peak amp
    // is bounded by ≈ 0.18 jump per sample at most, even at the steepest
    // zero-crossing. We're stricter: assert max |Δx| < 0.05 over the choke
    // window so the test fails loudly the moment the click reappears.
    let window_start = choke_sample.saturating_sub(8);
    let window_end = (choke_sample + 600).min(all_l.len());
    let mut max_jump = 0.0_f32;
    let mut max_jump_at = 0usize;
    let mut max_jump_channel = "?";
    for ch_name in ["L", "R"] {
        let ch = if ch_name == "L" { &all_l } else { &all_r };
        for i in (window_start + 1)..window_end {
            let d = (ch[i] - ch[i - 1]).abs();
            if d > max_jump {
                max_jump = d;
                max_jump_at = i;
                max_jump_channel = ch_name;
            }
        }
    }
    let pre_choke_peak = all_l[choke_sample.saturating_sub(64)..choke_sample]
        .iter()
        .map(|x| x.abs())
        .fold(0.0f32, f32::max)
        .max(
            all_r[choke_sample.saturating_sub(64)..choke_sample]
                .iter()
                .map(|x| x.abs())
                .fold(0.0f32, f32::max),
        );

    eprintln!(
        "Pre-choke peak amplitude: {pre_choke_peak:.4}; \
         max |Δx| in choke window ({}..{}): {max_jump:.4} @ sample {max_jump_at} ({max_jump_channel})",
        window_start, window_end
    );

    assert!(
        max_jump < 0.05,
        "Choke produces a click: |Δx|={max_jump:.4} at sample {max_jump_at} ({max_jump_channel}) — \
         pre-choke peak was {pre_choke_peak:.4}. The envelope is hard-cut instead of faded out."
    );
}
