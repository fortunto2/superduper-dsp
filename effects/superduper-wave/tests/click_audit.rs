//! click_audit.rs — drive Wave through a voice-steal-heavy MIDI sequence and
//! measure sample-to-sample discontinuities (clicks).
//!
//! The user reported a *barely audible* click when playing fast — i.e. when
//! new notes arrive while all 8 voices are busy and the allocator has to
//! **steal**. Two patches are probed:
//!   A. default-ish (FEnv Amt = 0) — isolates the "pitch changes at preserved
//!      oscillator phase" slope-kink that any hard steal produces.
//!   B. filter patch (FEnv Amt = +3 oct, Reson 0.5, FEnv Sustain 0.5) — exposes
//!      whether stealing resets the filter envelope to 0 (a cutoff step at full
//!      amplitude → an audible click).
//!
//! Writes /tmp/wave_click_audit_{plain,filter}.wav for listening. Prints the
//! max single-sample jump, WHERE it happens (so it can be correlated with a
//! steal event), and a discontinuity histogram.
//!
//! Run: cargo test --release -p superduper-wave --test click_audit -- --nocapture
//! Listen: afplay /tmp/wave_click_audit_filter.wav

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
use superduper_wave::SuperDuperWave;

const SR: f32 = 48_000.0;
const BLOCK: u32 = 256; // small blocks → more event seams
const TOTAL_SECONDS: f32 = 5.0;

// Param ids (mirror lib.rs PARAMS).
const P_CUTOFF: u32 = 4;
const P_RESON: u32 = 5;
const P_OUTPUT: u32 = 12;
const P_FENV_AMT: u32 = 15;
const P_FENV_D: u32 = 17;
const P_FENV_S: u32 = 18;

#[derive(Clone, Copy)]
enum MidiAt {
    On(u8, f32),
    Off(u8),
}

/// 8-note held drone (fills the whole voice pool) + a burst of fast stabs that
/// forces the allocator to steal held voices at full amplitude.
fn build_sequence() -> Vec<(usize, MidiAt)> {
    let t = |s: f32| (s * SR) as usize;
    let mut seq = Vec::new();
    // Phase 1 — build an 8-note drone cluster (fills all 8 voices).
    let drone: [u8; 8] = [36, 40, 43, 48, 52, 55, 60, 64];
    for (i, k) in drone.iter().enumerate() {
        seq.push((t(0.10 + i as f32 * 0.08), MidiAt::On(*k, 0.85)));
    }
    // Phase 2 — hold the drone, rain fast stabs on top → every stab must steal
    // a held (full-amplitude, past-decay) voice. This is the worst case.
    let stab_start = 2.0;
    let stab_keys: [u8; 16] = [
        67, 69, 72, 74, 76, 77, 79, 81, 72, 71, 69, 67, 65, 64, 62, 60,
    ];
    for (i, k) in stab_keys.iter().enumerate() {
        let on = stab_start + i as f32 * 0.06;
        seq.push((t(on), MidiAt::On(*k, 0.9)));
        seq.push((t(on + 0.045), MidiAt::Off(*k)));
    }
    // Release the drone at the end.
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

/// Render the sequence with a given patch. `params` = (id, value) set at block 0.
fn render(params: &[(u32, f64)]) -> (Vec<f32>, Vec<f32>) {
    // Boot the factory sine, not the user's last.json.
    std::env::set_var("SUPERDUPER_WAVE_FACTORY", "1");
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperWave>>(
        c"/in/process/test/superduper-wave-click",
    )
    .expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin =
        PluginInstance::<TH>::new(|_| TS, |_| (), &entry, c"co.superduperai.wave", &host_info)
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

/// Anomaly-based click detector. A click is a jump that's much larger than the
/// *local* per-sample slope, not just large in absolute terms (an 8-voice
/// cluster legitimately has steep slopes). For each sample we compute
/// |Δ[i]| / (median |Δ| over the surrounding ±W window). The biggest ratios are
/// the audible discontinuities; their times should line up with note events.
fn analyse(sig: &[f32]) -> (f32, usize, Vec<(f32, usize)>) {
    let n = sig.len();
    let d: Vec<f32> = (1..n).map(|i| (sig[i] - sig[i - 1]).abs()).collect();
    const W: usize = 512; // ~10 ms local context
    let max_jump = d.iter().cloned().fold(0.0f32, f32::max);
    let at = d.iter().position(|&x| x == max_jump).map(|i| i + 1).unwrap_or(0);
    // Candidates: only the meaningfully large jumps get the (costlier) local-
    // median ratio, so this stays fast even in a debug build.
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

fn probe(label: &str, wav: &str, params: &[(u32, f64)]) -> f32 {
    let (l, r) = render(params);
    write_wav(wav, &l, &r);
    let (mj_l, at_l, anom) = analyse(&l);
    eprintln!(
        "[{label}] max raw jump = {:.4} at t={:.3}s  → {wav}",
        mj_l,
        at_l as f32 / SR,
    );
    eprintln!("  top anomalous discontinuities (|Δ|/local-median, must be low):");
    for (ratio, idx) in &anom {
        eprintln!(
            "    ratio {:5.1}×  at t={:.3}s  |Δ|={:.4}",
            ratio,
            *idx as f32 / SR,
            (l[*idx] - l[*idx - 1]).abs()
        );
    }
    // Dump the sample context around the worst anomaly so its shape is visible
    // (single step vs multi-sample glitch vs ramp).
    if let Some((_, idx)) = anom.first() {
        let a = idx.saturating_sub(10);
        let b = (idx + 10).min(l.len());
        eprint!("  L context around worst @{}: ", idx);
        for i in a..b {
            let mark = if i == *idx { "*" } else { "" };
            eprint!("{mark}{:.4} ", l[i]);
        }
        eprintln!();
    }
    // Return the worst anomaly ratio as the click score.
    anom.first().map(|(r, _)| *r).unwrap_or(0.0)
}

#[test]
fn wave_click_audit_steal() {
    // A — default-ish patch: no filter-env modulation. Isolates the raw
    // steal (pitch change at preserved phase) discontinuity.
    let plain = probe(
        "plain",
        "/tmp/wave_click_audit_plain.wav",
        &[(P_OUTPUT, -6.0)],
    );

    // B — realistic filter bass/drone patch. FEnv sweeps the cutoff; a steal
    // that resets filter_env to 0 shows up here as a cutoff step click.
    let filter = probe(
        "filter",
        "/tmp/wave_click_audit_filter.wav",
        &[
            (P_OUTPUT, -6.0),
            (P_CUTOFF, 700.0),
            (P_RESON, 0.5),
            (P_FENV_AMT, 3.0),
            (P_FENV_D, 0.5),
            (P_FENV_S, 0.5),
        ],
    );

    // A click shows up as a discontinuity many times the local slope. With the
    // deferred-steal fade the worst full-amplitude swap discontinuities (~30×,
    // |Δ|≈0.16) are gone; what remains sits under ~20× at dense polyphonic
    // peaks. Guard against a regression back to the hard-swap behaviour.
    // With deferred-steal + legato retrigger the discontinuities at note events
    // are gone; what the detector still flags (~6-7×) is smooth zero-crossings
    // of the 8-voice sum, not clicks. A regression to any hard-swap/gate_on
    // path brings back 17-30×.
    eprintln!("worst anomaly ratios: plain={plain:.1}× filter={filter:.1}×");
    assert!(
        plain < 12.0 && filter < 12.0,
        "voice click regressed (hard swap / gate_on retrigger?): plain={plain:.1}×, filter={filter:.1}× (want < 12)"
    );
}
