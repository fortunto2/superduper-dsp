//! click_audit.rs — drive Pad through a voice-steal + same-key-retrigger
//! sequence and measure sample-to-sample discontinuities (clicks).
//!
//! Same class of bug fixed in Wave/Kubyz: over a sustained pad, fast notes
//! steal busy voices and retrigger held keys. The old code routed both through
//! `gate_on` (PreDelay zeroes the level → a drop-to-zero click) and hard-swapped
//! stolen voices at full amplitude. This asserts those are gone. Writes
//! /tmp/pad_click_audit.wav for listening.
//!
//! Run: cargo test --release -p superduper-pad --test click_audit -- --nocapture
//! Listen: afplay /tmp/pad_click_audit.wav

use clack_common::events::Pckn;
use clack_common::events::event_types::ParamValueEvent;
use clack_common::utils::{ClapId, Cookie};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, AudioPorts};
use clack_plugin::entry::SinglePluginEntry;
use std::io::Write;
use superduper_pad::SuperDuperPad;

const SR: f32 = 48_000.0;
const BLOCK: u32 = 256;
const TOTAL_SECONDS: f32 = 5.0;

const P_ATTACK: u32 = 5;
const P_SUSTAIN: u32 = 7;
const P_RELEASE: u32 = 8;

#[derive(Clone, Copy)]
enum MidiAt {
    On(u8, f32),
    Off(u8),
}

fn build_sequence() -> Vec<(usize, MidiAt)> {
    let t = |s: f32| (s * SR) as usize;
    let mut seq = Vec::new();
    let drone: [u8; 8] = [36, 40, 43, 48, 52, 55, 60, 64];
    for (i, k) in drone.iter().enumerate() {
        seq.push((t(0.10 + i as f32 * 0.08), MidiAt::On(*k, 0.85)));
    }
    let stab_start = 2.0;
    let stab_keys: [u8; 16] = [
        67, 69, 72, 74, 76, 77, 79, 81, 72, 71, 69, 67, 65, 64, 62, 60,
    ];
    for (i, k) in stab_keys.iter().enumerate() {
        let on = stab_start + i as f32 * 0.06;
        seq.push((t(on), MidiAt::On(*k, 0.9)));
        seq.push((t(on + 0.045), MidiAt::Off(*k)));
    }
    for k in drone.iter() {
        seq.push((t(4.2), MidiAt::Off(*k)));
    }
    seq.sort_by_key(|(p, _)| *p);
    seq
}

struct TS;
impl SharedHandler<'_> for TS {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}
impl HostLogImpl for TS {
    fn log(&self, _: LogSeverity, _: &str) {}
}
struct TH;
impl HostHandlers for TH {
    type Shared<'a> = TS;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
    fn declare_extensions(b: &mut HostExtensions<Self>, _: &Self::Shared<'_>) {
        b.register::<HostLog>();
    }
}

fn render(params: &[(u32, f64)]) -> (Vec<f32>, Vec<f32>) {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperPad>>(
        c"/in/process/test/superduper-pad-click",
    )
    .expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin =
        PluginInstance::<TH>::new(|_| TS, |_| (), &entry, c"co.superduperai.pad", &host_info)
            .expect("instantiate");
    let total_frames = (SR * TOTAL_SECONDS) as usize;
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

    let sequence = build_sequence();
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
                    for &(id, v) in params {
                        in_buf.push(&ParamValueEvent::new(
                            0,
                            ClapId::new(id),
                            Pckn::new(0u16, 0u16, 0u16, 0u32),
                            v,
                            Cookie::empty(),
                        ));
                    }
                }
                for (pos, ev) in &sequence {
                    if *pos >= start && *pos < end {
                        let local = (*pos - start) as u32;
                        match *ev {
                            MidiAt::On(key, vel) => {
                                in_buf.push(&NoteOnEvent::new(
                                    local,
                                    Pckn::new(0u16, 0u16, key as u16, 0u32),
                                    vel as f64,
                                ));
                            }
                            MidiAt::Off(key) => {
                                in_buf.push(&NoteOffEvent::new(
                                    local,
                                    Pckn::new(0u16, 0u16, key as u16, 0u32),
                                    1.0,
                                ));
                            }
                        }
                    }
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
        })
        .join()
        .expect("audio thread")
    });
    plugin.deactivate(stopped_back);
    (all_l, all_r)
}

fn write_wav(path: &str, l: &[f32], r: &[f32]) {
    let n = l.len();
    let data_size = (n as u32) * 2 * 2;
    let file_size = 36 + data_size;
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(b"RIFF").unwrap();
    f.write_all(&file_size.to_le_bytes()).unwrap();
    f.write_all(b"WAVE").unwrap();
    f.write_all(b"fmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap();
    f.write_all(&1u16.to_le_bytes()).unwrap();
    f.write_all(&2u16.to_le_bytes()).unwrap();
    f.write_all(&(SR as u32).to_le_bytes()).unwrap();
    f.write_all(&((SR as u32) * 4).to_le_bytes()).unwrap();
    f.write_all(&4u16.to_le_bytes()).unwrap();
    f.write_all(&16u16.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&data_size.to_le_bytes()).unwrap();
    for i in 0..n {
        let li = (l[i].clamp(-1.0, 1.0) * 32767.0) as i16;
        let ri = (r[i].clamp(-1.0, 1.0) * 32767.0) as i16;
        f.write_all(&li.to_le_bytes()).unwrap();
        f.write_all(&ri.to_le_bytes()).unwrap();
    }
}

fn analyse(sig: &[f32]) -> (f32, usize, Vec<(f32, usize)>) {
    let n = sig.len();
    let d: Vec<f32> = (1..n).map(|i| (sig[i] - sig[i - 1]).abs()).collect();
    const W: usize = 512;
    let max_jump = d.iter().cloned().fold(0.0f32, f32::max);
    let at = d.iter().position(|&x| x == max_jump).map(|i| i + 1).unwrap_or(0);
    let mut anomalies: Vec<(f32, usize)> = Vec::new();
    for i in 0..d.len() {
        if d[i] < 0.02 {
            continue;
        }
        let lo = i.saturating_sub(W);
        let hi = (i + W).min(d.len());
        let mut win: Vec<f32> = d[lo..hi].to_vec();
        let mid = win.len() / 2;
        win.select_nth_unstable_by(mid, f32::total_cmp);
        let local = win[mid].max(1e-6);
        let ratio = d[i] / local;
        if ratio > 6.0 {
            anomalies.push((ratio, i + 1));
        }
    }
    anomalies.sort_by(|a, b| b.0.total_cmp(&a.0));
    anomalies.truncate(6);
    (max_jump, at, anomalies)
}

#[test]
fn pad_click_audit_steal_retrigger() {
    let (l, r) = render(&[(P_ATTACK, 0.02), (P_SUSTAIN, 0.85), (P_RELEASE, 1.5)]);
    write_wav("/tmp/pad_click_audit.wav", &l, &r);
    // Only whole blocks are rendered; the trailing partial block stays zero.
    // Pad's 1.5 s release is still sounding at that boundary, so analyse only
    // the rendered region — otherwise the step into the zero pad reads as a
    // (spurious) click.
    let rendered = (l.len() / BLOCK as usize) * BLOCK as usize;
    let (mj, at, anom) = analyse(&l[..rendered]);
    eprintln!(
        "[pad] max raw jump = {:.4} at t={:.3}s  → /tmp/pad_click_audit.wav",
        mj,
        at as f32 / SR
    );
    for (ratio, idx) in &anom {
        eprintln!(
            "    ratio {:5.1}×  at t={:.3}s  |Δ|={:.4}",
            ratio,
            *idx as f32 / SR,
            (l[*idx] - l[*idx - 1]).abs()
        );
    }
    let worst = anom.first().map(|(r, _)| *r).unwrap_or(0.0);
    eprintln!("worst anomaly ratio: {worst:.1}×");
    assert!(
        worst < 12.0,
        "pad voice click regressed (hard swap / gate_on retrigger?): {worst:.1}× (want < 12)"
    );
}
