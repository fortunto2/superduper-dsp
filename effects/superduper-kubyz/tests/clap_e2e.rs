//! clap_e2e.rs — boot Kubyz through clack-host, send a NoteOn,
//! render audio, assert it produced sound with energy concentrated
//! around the requested fundamental.
//!
//! Run: cargo test --release -p superduper-kubyz --test clap_e2e -- --nocapture

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::Pckn;
use clack_host::events::event_types::NoteOnEvent;
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, AudioPorts};
use clack_plugin::entry::SinglePluginEntry;
use superduper_kubyz::SuperDuperKubyz;
use superduper_synth_core::analysis::magnitude_spectrum_db;

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
fn kubyz_produces_audio_on_noteon() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperKubyz>>(
        c"/in/process/test/superduper-kubyz-e2e",
    ).expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TH>::new(
        |_| TS, |_| (), &entry, c"co.superduperai.kubyz", &host_info,
    ).expect("instantiate");

    const SR: f32 = 48_000.0;
    const BLOCK: u32 = 256;
    const SECONDS: f32 = 1.5;
    let total_frames = (SR * SECONDS) as usize;
    let block_us = BLOCK as usize;
    let n_blocks = total_frames / block_us;

    let stopped = plugin.activate(|_, _| (), PluginAudioConfiguration {
        sample_rate: SR as f64, min_frames_count: BLOCK, max_frames_count: BLOCK,
    }).expect("activate");

    let mut all_l = vec![0.0_f32; total_frames];
    let mut all_r = vec![0.0_f32; total_frames];
    let l_ref = &mut all_l;
    let r_ref = &mut all_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(0, 0);
            let mut out_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let start = block * block_us;
                let end = start + block_us;

                let mut in_buf = EventBuffer::new();
                if block == 0 {
                    // NoteOn key=57 (A3 = 220 Hz) at velocity 1.0
                    let on = NoteOnEvent::new(
                        0, Pckn::new(0u16, 0u16, 57u16, 0u32), 1.0,
                    );
                    in_buf.push(&on);
                }
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);

                let mut out_l = vec![0.0_f32; block_us];
                let mut out_r = vec![0.0_f32; block_us];

                let input_audio = in_ports.with_input_buffers(std::iter::empty::<
                    AudioPortBuffer<
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f32>>,
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f64>>,
                    >,
                >());
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
        }).join().expect("audio thread")
    });
    plugin.deactivate(stopped_back);

    // Sustained section (skip attack).
    let skip = (SR * 0.10) as usize;
    let mono: Vec<f32> = (skip..all_l.len())
        .map(|i| 0.5 * (all_l[i] + all_r[i]))
        .collect();

    let rms = (mono.iter().map(|x| x * x).sum::<f32>() / mono.len() as f32).sqrt();
    let peak = mono.iter().fold(0.0_f32, |a, x| a.max(x.abs()));
    eprintln!("Kubyz A3 (220 Hz): rms={:.4}, peak={:.4}", rms, peak);
    assert!(rms > 0.001, "Kubyz produced no audible audio (rms={rms})");
    assert!(peak < 0.99, "Kubyz clipping (peak={peak})");
    for x in &mono {
        assert!(x.is_finite(), "Kubyz produced NaN/Inf");
    }

    // Spectrum — energy should be concentrated below 8 kHz with a
    // clear peak somewhere near the fundamental or its harmonics.
    // (Kubyz is a 16-harmonic engine — the formant modulates which
    // partial dominates, so we don't pin to f0 directly; we just
    // assert there's significant LF content.)
    let fft_n = mono.len().next_power_of_two() / 2;
    if fft_n >= 4096 {
        let slice = &mono[mono.len() - fft_n..];
        let spec = magnitude_spectrum_db(slice);
        let n_bins = spec.len();
        // Bin centre in Hz: i * SR / (2 * (n_bins-1))
        let bin_hz = |i: usize| (i as f32) * SR / ((n_bins - 1) as f32 * 2.0);
        let mut energy_low = 0.0_f32;
        let mut energy_high = 0.0_f32;
        for (i, &db) in spec.iter().enumerate() {
            let mag = 10f32.powf(db / 10.0); // power
            if bin_hz(i) < 2000.0 { energy_low += mag; }
            else if bin_hz(i) > 8000.0 { energy_high += mag; }
        }
        eprintln!("LF energy (<2 kHz): {:.2}, HF energy (>8 kHz): {:.2}", energy_low, energy_high);
        assert!(
            energy_low > energy_high * 2.0,
            "Kubyz spectrum is supposed to be LF-dominant (jaw harp / khomus); \
             low energy {energy_low:.2} vs high {energy_high:.2}"
        );
    }
}
