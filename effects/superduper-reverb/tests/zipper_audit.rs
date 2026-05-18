//! zipper_audit.rs — sweep SIZE from 0.5 → 1.5 over 1 second while pink
//! noise plays through the reverb. Without continuous interpolation +
//! smoothing the integer tap reads click on every integer crossing; this
//! test asserts max sample-to-sample jump stays bounded and writes
//! /tmp/reverb_zipper_audit.wav for human audition.

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::Pckn;
use clack_host::events::event_types::ParamValueEvent;
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_host::utils::{ClapId, Cookie};
use clack_plugin::entry::SinglePluginEntry;
use std::io::Write;
use superduper_reverb::SuperDuperReverb;

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

fn write_wav_i16_stereo(path: &str, sr: u32, l: &[f32], r: &[f32]) -> std::io::Result<()> {
    assert_eq!(l.len(), r.len());
    let n = l.len();
    let data_size = (n * 4) as u32;
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_size).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&sr.to_le_bytes())?;
    f.write_all(&(sr * 4).to_le_bytes())?;
    f.write_all(&4u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_size.to_le_bytes())?;
    for i in 0..n {
        let li = (l[i].clamp(-1.0, 1.0) * 32767.0) as i16;
        let ri = (r[i].clamp(-1.0, 1.0) * 32767.0) as i16;
        f.write_all(&li.to_le_bytes())?;
        f.write_all(&ri.to_le_bytes())?;
    }
    Ok(())
}

/// Voss-McCartney pink-ish PRNG noise.
fn make_pink(n: usize, seed: u32) -> Vec<f32> {
    let mut state = seed;
    let mut out = Vec::with_capacity(n);
    let mut rows = [0.0_f32; 6];
    let mut counter: u32 = 0;
    for _ in 0..n {
        state ^= state << 13; state ^= state >> 17; state ^= state << 5;
        let r = ((state >> 8) & 0xFFFFFF) as f32 / 16_777_216.0 - 0.5;
        let tz = counter.trailing_zeros().min(5) as usize;
        rows[tz] = r;
        let sum: f32 = rows.iter().sum::<f32>() / rows.len() as f32;
        out.push(sum * 0.6);
        counter = counter.wrapping_add(1);
    }
    out
}

const SR: f32 = 48_000.0;
const BLOCK: u32 = 128;
const SECONDS: f32 = 2.5;
const SIZE_PARAM_ID: u32 = 0;
const MIX_PARAM_ID: u32 = 6;

#[test]
fn size_sweep_does_not_zipper() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperReverb>>(
        c"/in/process/test/superduper-reverb-zipper",
    )
    .expect("plugin entry should load");
    let host_info = HostInfo::new("SDSP Test", "SuperDuperAI", "https://superduperai.co", "0")
        .unwrap();
    let mut plugin = PluginInstance::<TestHost>::new(
        |_| TestHostShared,
        |_| (),
        &entry,
        c"co.superduperai.reverb",
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
    let total_frames: usize = (SR * SECONDS) as usize;
    let n_blocks = total_frames / BLOCK_USIZE;
    let mut in_l = make_pink(total_frames, 0xCAFE_BABE);
    let mut in_r = make_pink(total_frames, 0xDEAD_BEEF);
    // Pre-make all-zero sidechain — separate buffers per channel since
    // clack-host wants &mut.
    let mut sc_l = vec![0.0_f32; total_frames];
    let mut sc_r = vec![0.0_f32; total_frames];

    let sweep_start_block = ((SR * 0.5) as usize) / BLOCK_USIZE;
    let sweep_end_block = ((SR * 1.5) as usize) / BLOCK_USIZE;

    let mut out_l = vec![0.0_f32; total_frames];
    let mut out_r = vec![0.0_f32; total_frames];

    let in_l_ref = &mut in_l;
    let in_r_ref = &mut in_r;
    let sc_l_ref = &mut sc_l;
    let sc_r_ref = &mut sc_r;
    let out_l_ref = &mut out_l;
    let out_r_ref = &mut out_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut audio_proc = stopped.start_processing().expect("start");
            // 2 inputs (main + sidechain), 1 output.
            let mut input_ports = AudioPorts::with_capacity(2, 2);
            let mut output_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let start = block * BLOCK_USIZE;
                let end = start + BLOCK_USIZE;

                // ---- Events: force MIX=1.0 at block 0, sweep SIZE 0.5→1.5 ----
                let mut input_buf = EventBuffer::new();
                if block == 0 {
                    let ev = ParamValueEvent::new(
                        0,
                        ClapId::new(MIX_PARAM_ID),
                        Pckn::new(0u16, 0u16, 0u16, 0u32),
                        1.0,
                        Cookie::empty(),
                    );
                    input_buf.push(&ev);
                }
                if block >= sweep_start_block && block <= sweep_end_block {
                    let span = (sweep_end_block - sweep_start_block).max(1);
                    let prog = (block - sweep_start_block) as f32 / span as f32;
                    let size_value = (0.5 + prog) as f64;
                    let ev = ParamValueEvent::new(
                        0,
                        ClapId::new(SIZE_PARAM_ID),
                        Pckn::new(0u16, 0u16, 0u16, 0u32),
                        size_value,
                        Cookie::empty(),
                    );
                    input_buf.push(&ev);
                }
                let input_events = InputEvents::from_buffer(&input_buf);
                let mut out_evs = EventBuffer::new();
                let mut output_events = OutputEvents::from_buffer(&mut out_evs);

                // ---- Audio buffers ----
                let in_l_chunk = &mut in_l_ref[start..end];
                let in_r_chunk = &mut in_r_ref[start..end];
                let mut main_chans: [&mut [f32]; 2] = [in_l_chunk, in_r_chunk];

                let sc_l_chunk = &mut sc_l_ref[start..end];
                let sc_r_chunk = &mut sc_r_ref[start..end];
                let mut sc_chans: [&mut [f32]; 2] = [sc_l_chunk, sc_r_chunk];

                let mut out_l_chunk = vec![0.0_f32; BLOCK_USIZE];
                let mut out_r_chunk = vec![0.0_f32; BLOCK_USIZE];

                // Collect into Vec so both ports share the same iterator
                // type (`vec::IntoIter<InputChannel>`); two map closures
                // would be distinct types and clack-host's array binding
                // wants them identical.
                let main_in: Vec<InputChannel<f32>> = main_chans
                    .iter_mut()
                    .map(|b| InputChannel::variable(*b))
                    .collect();
                let sc_in: Vec<InputChannel<f32>> = sc_chans
                    .iter_mut()
                    .map(|b| InputChannel::variable(*b))
                    .collect();
                let input_audio = input_ports.with_input_buffers([
                    AudioPortBuffer {
                        latency: 0,
                        channels: AudioPortBufferType::f32_input_only(main_in.into_iter()),
                    },
                    AudioPortBuffer {
                        latency: 0,
                        channels: AudioPortBufferType::f32_input_only(sc_in.into_iter()),
                    },
                ]);

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
                    .expect("process");

                out_l_ref[start..end].copy_from_slice(&out_l_chunk);
                out_r_ref[start..end].copy_from_slice(&out_r_chunk);
            }
            audio_proc.stop_processing()
        })
        .join()
        .expect("audio thread")
    });
    plugin.deactivate(stopped_back);

    let wav = "/tmp/reverb_zipper_audit.wav";
    write_wav_i16_stereo(wav, SR as u32, &out_l, &out_r).expect("wav write");
    eprintln!("Wrote {wav}");

    let sweep_start_sample = sweep_start_block * BLOCK_USIZE;
    let sweep_end_sample = ((sweep_end_block + 1) * BLOCK_USIZE).min(out_l.len());
    let mut max_d = 0.0_f32;
    let mut max_d_at = 0usize;
    let mut max_d_ch = "?";
    for ch_name in ["L", "R"] {
        let ch = if ch_name == "L" { &out_l } else { &out_r };
        for i in (sweep_start_sample + 1)..sweep_end_sample {
            let d = (ch[i] - ch[i - 1]).abs();
            if d > max_d {
                max_d = d;
                max_d_at = i;
                max_d_ch = ch_name;
            }
        }
    }
    let pre_sweep_peak = out_l[..sweep_start_sample]
        .iter()
        .chain(out_r[..sweep_start_sample].iter())
        .map(|x| x.abs())
        .fold(0.0_f32, f32::max);
    eprintln!(
        "Pre-sweep peak: {pre_sweep_peak:.4}; max |Δx| in sweep window: {max_d:.4} @ sample {max_d_at} ({max_d_ch})"
    );

    assert!(
        max_d < 0.25,
        "SIZE sweep introduces audible discontinuity: max |Δx|={max_d:.4} at sample {max_d_at} ({max_d_ch}). \
         Pre-sweep peak was {pre_sweep_peak:.4}. Audit /tmp/reverb_zipper_audit.wav."
    );
}
