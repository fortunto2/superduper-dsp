//! An in-process CLAP host, once, instead of once per plugin.
//!
//! This is the harness that every `clap_e2e.rs` was carrying its own copy of.
//! Nothing here is clever — it is the same activate / start_processing / block
//! loop — but having it in one place is what makes a quality suite per plugin
//! cheap enough that all thirty can have one.

use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_plugin::entry::SinglePluginEntry;

/// What the kit needs to know to drive a plugin.
pub trait PluginUnderTest {
    /// The plugin type, as passed to `SinglePluginEntry`.
    type Plugin: clack_plugin::prelude::DefaultPluginFactory + 'static;
    /// Its CLAP id, e.g. `c"co.superduperai.saturator"`.
    const ID: &'static std::ffi::CStr;
}

struct KitHostShared;

impl SharedHandler<'_> for KitHostShared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}

impl clack_extensions::log::HostLogImpl for KitHostShared {
    fn log(&self, severity: clack_extensions::log::LogSeverity, message: &str) {
        eprintln!("[plugin {severity}] {message}");
    }
}

struct KitHost;

impl HostHandlers for KitHost {
    type Shared<'a> = KitHostShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();

    fn declare_extensions(
        builder: &mut HostExtensions<Self>,
        _: &Self::Shared<'_>,
    ) {
        builder.register::<clack_extensions::log::HostLog>();
    }
}

/// Run a stereo signal through a plugin and return its output.
///
/// `events` is called once per block with the block's start frame, so a caller
/// can inject notes or parameter changes at a known position.
pub fn render_with<P: PluginUnderTest>(
    sr: f64,
    block: usize,
    input_l: &[f32],
    input_r: &[f32],
    mut events: impl FnMut(usize, &mut EventBuffer) + Send,
) -> (Vec<f32>, Vec<f32>) {
    let entry =
        PluginEntry::load_from_clack::<SinglePluginEntry<P::Plugin>>(c"/in/process/sdsp-test-kit")
            .expect("plugin entry loads");
    let host_info =
        HostInfo::new("SuperDuper Test Kit", "SuperDuperAI", "https://superduperai.co", "0.1")
            .unwrap();
    let mut instance =
        PluginInstance::<KitHost>::new(|_| KitHostShared, |_| (), &entry, P::ID, &host_info)
            .expect("plugin instantiates");

    let frames = input_l.len().min(input_r.len());
    let cfg = PluginAudioConfiguration {
        sample_rate: sr,
        min_frames_count: block as u32,
        max_frames_count: block as u32,
    };
    let stopped = instance.activate(|_, _| (), cfg).expect("activate");

    let mut out_l = vec![0.0_f32; frames];
    let mut out_r = vec![0.0_f32; frames];

    // The API insists start_processing happens in an audio-thread context.
    let (ol, or) = (&mut out_l, &mut out_r);
    let stopped = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start_processing");
            let mut in_ports = AudioPorts::with_capacity(2, 1);
            let mut out_ports = AudioPorts::with_capacity(2, 1);

            let mut pos = 0;
            while pos + block <= frames {
                let mut in_l: Vec<f32> = input_l[pos..pos + block].to_vec();
                let mut in_r: Vec<f32> = input_r[pos..pos + block].to_vec();
                let mut chans: [&mut [f32]; 2] = [&mut in_l, &mut in_r];

                let mut ev_buf = EventBuffer::new();
                events(pos, &mut ev_buf);
                let input_events = ev_buf.as_input();
                let mut out_ev = EventBuffer::new();
                let mut output_events = OutputEvents::from_buffer(&mut out_ev);

                let audio_in = in_ports.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(
                        chans.iter_mut().map(|b| InputChannel::variable(*b)),
                    ),
                }]);

                let mut blk_l = vec![0.0_f32; block];
                let mut blk_r = vec![0.0_f32; block];
                let mut out_chans: [&mut [f32]; 2] = [&mut blk_l, &mut blk_r];
                let mut audio_out = out_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        out_chans.iter_mut().map(|b| &mut **b),
                    ),
                }]);

                proc.process(&audio_in, &mut audio_out, &input_events, &mut output_events, None, None)
                    .expect("process");

                ol[pos..pos + block].copy_from_slice(&blk_l);
                or[pos..pos + block].copy_from_slice(&blk_r);
                pos += block;
            }
            proc.stop_processing()
        })
        .join()
        .expect("audio thread")
    });
    drop(stopped);
    (out_l, out_r)
}

/// Effects: signal in, signal out, no events.
pub fn render_effect<P: PluginUnderTest>(
    sr: f64,
    input_l: &[f32],
    input_r: &[f32],
) -> (Vec<f32>, Vec<f32>) {
    render_with::<P>(sr, 512, input_l, input_r, |_, _| {})
}

/// Instruments: silence in, one note held for `hold_s` of a `total_s` render.
pub fn render_instrument<P: PluginUnderTest>(
    sr: f64,
    key: u8,
    hold_s: f64,
    total_s: f64,
) -> (Vec<f32>, Vec<f32>) {
    use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent};
    use clack_host::events::Pckn;

    let frames = (sr * total_s) as usize;
    let block = 512;
    let silence = vec![0.0_f32; frames];
    let off_at = (sr * hold_s) as usize;

    render_with::<P>(sr, block, &silence, &silence, move |pos, buf| {
        if pos == 0 {
            buf.push(&NoteOnEvent::new(0, Pckn::new(0u16, 0u16, key as u16, 0u32), 0.8));
        }
        if pos <= off_at && off_at < pos + block {
            buf.push(&NoteOffEvent::new(
                (off_at - pos) as u32,
                Pckn::new(0u16, 0u16, key as u16, 0u32),
                0.8,
            ));
        }
    })
}
