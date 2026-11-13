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

            // Allocated once, with room to grow, and cleared per block. Created
            // fresh inside the loop they grew on their first push — which
            // happens *inside* the plugin's process(), so the allocation
            // counter blamed the plugin for the harness's own buffer growth.
            let mut ev_buf = EventBuffer::with_capacity(256);
            let mut out_ev = EventBuffer::with_capacity(256);

            let mut pos = 0;
            while pos + block <= frames {
                let mut in_l: Vec<f32> = input_l[pos..pos + block].to_vec();
                let mut in_r: Vec<f32> = input_r[pos..pos + block].to_vec();
                let mut chans: [&mut [f32]; 2] = [&mut in_l, &mut in_r];

                ev_buf.clear();
                out_ev.clear();
                events(pos, &mut ev_buf);
                let input_events = ev_buf.as_input();
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

                // Only the plugin's own process() is inside the bracket; the
                // buffer shuffling above and below is the host's business.
                //
                // The first two blocks are not counted. Plugins legitimately do
                // one-time work on the first callback — Wave swaps in its
                // initial wavetable, the Sampler settles its library — and
                // those frees are not the per-block churn this is looking for.
                let counting = pos >= 2 * block;
                if counting {
                    crate::alloc::enter_rt();
                }
                let status = proc.process(
                    &audio_in, &mut audio_out, &input_events, &mut output_events, None, None,
                );
                if counting {
                    crate::alloc::exit_rt();
                }
                status.expect("process");

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

/// What the host tells the plugin about the transport for one block.
#[derive(Debug, Clone, Copy)]
pub struct TransportSpec {
    pub playing: bool,
    pub pos_seconds: f64,
    pub tempo: f64,
}

/// A note event the plugin emitted, at a global frame position.
#[derive(Debug, Clone, PartialEq)]
pub struct OutNote {
    pub frame: usize,
    pub on: bool,
    pub channel: u16,
    pub key: u16,
    pub velocity: f64,
}

fn transport_event(t: &TransportSpec) -> clack_plugin::events::event_types::TransportEvent {
    use clap_sys::events::*;
    const SEC: f64 = (1u64 << 31) as f64;
    let mut flags = CLAP_TRANSPORT_HAS_TEMPO | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE;
    if t.playing {
        flags |= CLAP_TRANSPORT_IS_PLAYING;
    }
    let raw = clap_event_transport {
        header: clap_event_header {
            size: std::mem::size_of::<clap_event_transport>() as u32,
            time: 0,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_TRANSPORT,
            flags: 0,
        },
        flags,
        song_pos_beats: 0,
        song_pos_seconds: (t.pos_seconds * SEC) as i64,
        tempo: t.tempo,
        tempo_inc: 0.0,
        loop_start_beats: 0,
        loop_end_beats: 0,
        loop_start_seconds: 0,
        loop_end_seconds: 0,
        bar_start: 0,
        bar_number: 0,
        tsig_num: 4,
        tsig_denom: 4,
    };
    clack_plugin::events::event_types::TransportEvent::from_raw(&raw)
}

/// Like [`render_with`], with a transport per block and the plugin's note output collected.
/// `transport(start_frame)` is asked once per block; `None` = no transport info at all.
/// `process()` runs inside the allocation counter's bracket from the third block on.
pub fn render_session<P: PluginUnderTest>(
    sr: f64,
    block: usize,
    input_l: &[f32],
    input_r: &[f32],
    mut events: impl FnMut(usize, &mut EventBuffer) + Send,
    mut transport: impl FnMut(usize) -> Option<TransportSpec> + Send,
) -> (Vec<f32>, Vec<f32>, Vec<OutNote>) {
    use clack_host::events::spaces::CoreEventSpace;
    let entry =
        PluginEntry::load_from_clack::<SinglePluginEntry<P::Plugin>>(c"/in/process/sdsp-test-kit")
            .expect("plugin entry loads");
    let host_info =
        HostInfo::new("SuperDuper Test Kit", "SuperDuperAI", "https://superduperai.co", "0.1").unwrap();
    let mut instance =
        PluginInstance::<KitHost>::new(|_| KitHostShared, |_| (), &entry, P::ID, &host_info)
            .expect("plugin instantiates");
    let frames = input_l.len().min(input_r.len());
    let cfg = PluginAudioConfiguration { sample_rate: sr, min_frames_count: block as u32, max_frames_count: block as u32 };
    let stopped = instance.activate(|_, _| (), cfg).expect("activate");
    let mut out_l = vec![0.0_f32; frames];
    let mut out_r = vec![0.0_f32; frames];
    let mut notes: Vec<OutNote> = Vec::new();

    let (ol, or, nt) = (&mut out_l, &mut out_r, &mut notes);
    let stopped = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start_processing");
            let mut in_ports = AudioPorts::with_capacity(2, 1);
            let mut out_ports = AudioPorts::with_capacity(2, 1);
            let mut ev_buf = EventBuffer::with_capacity(256);
            let mut out_ev = EventBuffer::with_capacity(1024);
            let mut pos = 0;
            while pos + block <= frames {
                let mut in_l: Vec<f32> = input_l[pos..pos + block].to_vec();
                let mut in_r: Vec<f32> = input_r[pos..pos + block].to_vec();
                let mut chans: [&mut [f32]; 2] = [&mut in_l, &mut in_r];
                ev_buf.clear();
                out_ev.clear();
                events(pos, &mut ev_buf);
                let tr = transport(pos).map(|t| transport_event(&t));
                let input_events = ev_buf.as_input();
                let mut output_events = OutputEvents::from_buffer(&mut out_ev);
                let audio_in = in_ports.with_input_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_input_only(chans.iter_mut().map(|b| InputChannel::variable(*b))),
                }]);
                let mut blk_l = vec![0.0_f32; block];
                let mut blk_r = vec![0.0_f32; block];
                let mut out_chans: [&mut [f32]; 2] = [&mut blk_l, &mut blk_r];
                let mut audio_out = out_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(out_chans.iter_mut().map(|b| &mut **b)),
                }]);
                let counting = pos >= 2 * block;
                if counting {
                    crate::alloc::enter_rt();
                }
                let status = proc.process(&audio_in, &mut audio_out, &input_events, &mut output_events, None, tr.as_ref());
                if counting {
                    crate::alloc::exit_rt();
                }
                status.expect("process");
                for ev in out_ev.iter() {
                    match ev.as_core_event() {
                        Some(CoreEventSpace::NoteOn(n)) => nt.push(OutNote {
                            frame: pos + n.header().time() as usize,
                            on: true,
                            channel: n.channel().into_specific().unwrap_or(0),
                            key: n.key().into_specific().unwrap_or(0),
                            velocity: n.velocity(),
                        }),
                        Some(CoreEventSpace::NoteOff(n)) => nt.push(OutNote {
                            frame: pos + n.header().time() as usize,
                            on: false,
                            channel: n.channel().into_specific().unwrap_or(0),
                            key: n.key().into_specific().unwrap_or(0),
                            velocity: n.velocity(),
                        }),
                        _ => {}
                    }
                }
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
    (out_l, out_r, notes)
}
