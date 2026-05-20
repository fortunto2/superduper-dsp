//! sdsp-chain — headless multi-plugin chain runner.
//!
//! Reads a WAV file, pushes it through a configurable chain of
//! SuperDuper CLAP plugins (statically linked — no dynamic .clap
//! loading), writes the rendered output WAV, prints per-stage
//! LUFS-I + dBTP measurements.
//!
//! Usage:
//!     sdsp-chain chain.toml input.wav output.wav
//!
//! Config (`chain.toml`):
//! ```toml
//! [[stage]]
//! plugin = "eq"
//! # Params by positional id (string keys, float values).
//! params = { "1" = 1.0, "3" = -1.0 }
//!
//! [[stage]]
//! plugin = "compressor"
//! params = { "0" = -18.0 }
//!
//! [[stage]]
//! plugin = "limiter"
//! params = { "1" = -1.0 }
//! ```
//!
//! Supported plugin keys (v0): eq, compressor, saturator, midside,
//! limiter, lineq. Adding a plugin = add one match arm + crate dep.

use std::path::PathBuf;

use clack_common::events::Pckn;
use clack_common::events::event_types::ParamValueEvent;
use clack_common::utils::{ClapId, Cookie};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_plugin::entry::SinglePluginEntry;
use serde::Deserialize;

use superduper_synth_core::loudness::{LoudnessMeter, TruePeakDetector};
use superduper_synth_core::wav::{parse_wav_file, write_mono_f32_wav};

const SR: f64 = 44_100.0;
const BLOCK: u32 = 256;

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

// ===========================================================================
// Per-plugin processor — one function per supported plugin. Each runs
// the same boilerplate: instantiate, activate, push N blocks through
// `process()`, deactivate. Macro to keep the body identical across
// plugins.
// ===========================================================================

macro_rules! impl_stage {
    ($fn_name:ident, $plugin_ty:path, $bundle_id:literal, $entry_path:literal) => {
        fn $fn_name(
            params_cfg: &toml::Table,
            in_l: &[f32],
            in_r: &[f32],
        ) -> (Vec<f32>, Vec<f32>) {
            let entry = PluginEntry::load_from_clack::<SinglePluginEntry<$plugin_ty>>(
                concat_cstr!($entry_path),
            )
            .expect("entry");
            let host_info = HostInfo::new("sdsp-chain", "SuperDuperAI", "https://sdai.co", "0.1")
                .unwrap();
            let mut plugin = PluginInstance::<TH>::new(
                |_| TS,
                |_| (),
                &entry,
                concat_cstr!($bundle_id),
                &host_info,
            )
            .expect("instantiate");
            let n = in_l.len().min(in_r.len());
            let block = BLOCK as usize;
            let n_blocks = n / block;
            let stopped = plugin
                .activate(|_, _| (), PluginAudioConfiguration {
                    sample_rate: SR,
                    min_frames_count: BLOCK,
                    max_frames_count: BLOCK,
                })
                .expect("activate");

            let mut out_l = vec![0.0f32; n];
            let mut out_r = vec![0.0f32; n];

            // Build initial param-override event buffer.
            let mut init_events = EventBuffer::new();
            for (key, val) in params_cfg.iter() {
                if let (Ok(id), Some(v)) = (key.parse::<u32>(), val.as_float()) {
                    let ev = ParamValueEvent::new(
                        0,
                        ClapId::new(id),
                        Pckn::new(0u16, 0u16, 0u16, 0u32),
                        v,
                        Cookie::empty(),
                    );
                    init_events.push(&ev);
                }
            }

            let in_l_ref = in_l;
            let in_r_ref = in_r;
            let out_l_ref = &mut out_l;
            let out_r_ref = &mut out_r;
            let events_ref = &init_events;
            let stopped_back = std::thread::scope(|s| {
                s.spawn(move || {
                    let mut proc = stopped.start_processing().expect("start");
                    let mut in_ports = AudioPorts::with_capacity(2, 1);
                    let mut out_ports = AudioPorts::with_capacity(2, 1);
                    for blk in 0..n_blocks {
                        let start = blk * block;
                        let end = start + block;
                        let mut chunk_l = in_l_ref[start..end].to_vec();
                        let mut chunk_r = in_r_ref[start..end].to_vec();
                        let in_chans = [
                            InputChannel::variable(chunk_l.as_mut_slice()),
                            InputChannel::variable(chunk_r.as_mut_slice()),
                        ];
                        let input_audio = in_ports.with_input_buffers([AudioPortBuffer {
                            latency: 0,
                            channels: AudioPortBufferType::f32_input_only(in_chans.into_iter()),
                        }]);
                        let l_buf = &mut out_l_ref[start..end];
                        let r_buf = &mut out_r_ref[start..end];
                        let mut out_chans: [&mut [f32]; 2] = [l_buf, r_buf];
                        let mut output_audio = out_ports.with_output_buffers([AudioPortBuffer {
                            latency: 0,
                            channels: AudioPortBufferType::f32_output_only(
                                out_chans.iter_mut().map(|b| &mut **b),
                            ),
                        }]);
                        let inputs: InputEvents<'_> = if blk == 0 {
                            InputEvents::from_buffer(events_ref)
                        } else {
                            InputEvents::empty()
                        };
                        let mut out_evs = EventBuffer::new();
                        let mut outputs = OutputEvents::from_buffer(&mut out_evs);
                        proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None)
                            .expect("process");
                    }
                    proc.stop_processing()
                })
                .join()
                .expect("audio thread")
            });
            plugin.deactivate(stopped_back);
            (out_l, out_r)
        }
    };
}

/// Const-fn workaround for `&'static CStr` from a string literal —
/// the c"…" macro returns a non-static lifetime in clack-host APIs;
/// this wrapper makes it 'static via the bytes_with_nul path.
macro_rules! concat_cstr {
    ($lit:literal) => {{
        // SAFETY: literal is ASCII and we append NUL.
        unsafe {
            std::ffi::CStr::from_bytes_with_nul_unchecked(concat!($lit, "\0").as_bytes())
        }
    }};
}

impl_stage!(stage_eq,         superduper_eq::SuperDuperEq,                 "co.superduperai.eq",         "/sdsp-chain/eq");
impl_stage!(stage_lineq,      superduper_lineq::SuperDuperLinEq,           "co.superduperai.lineq",      "/sdsp-chain/lineq");
impl_stage!(stage_compressor, superduper_compressor::SuperDuperCompressor, "co.superduperai.compressor", "/sdsp-chain/comp");
impl_stage!(stage_saturator,  superduper_saturator::SuperDuperSaturator,   "co.superduperai.saturator",  "/sdsp-chain/sat");
impl_stage!(stage_limiter,    superduper_limiter::SuperDuperLimiter,       "co.superduperai.limiter",    "/sdsp-chain/lim");
impl_stage!(stage_midside,    superduper_midside::SuperDuperMidSide,       "co.superduperai.midside",    "/sdsp-chain/ms");
impl_stage!(stage_vocal,      superduper_vocal::SuperDuperVocal,           "co.superduperai.vocal",      "/sdsp-chain/voc");
impl_stage!(stage_filter,     superduper_filter::SuperDuperFilter,         "co.superduperai.filter",     "/sdsp-chain/flt");
impl_stage!(stage_reverb,     superduper_reverb::SuperDuperReverb,         "co.superduperai.reverb",     "/sdsp-chain/rev");
impl_stage!(stage_supermass,  superduper_supermass::SuperDuperSupermass,   "co.superduperai.supermass",  "/sdsp-chain/sm");
impl_stage!(stage_delay,      superduper_delay::SuperDuperDelay,           "co.superduperai.delay",      "/sdsp-chain/dly");
impl_stage!(stage_chorus,     superduper_chorus::SuperDuperChorus,         "co.superduperai.chorus",     "/sdsp-chain/cho");

fn dispatch(name: &str, cfg: &toml::Table, l: &[f32], r: &[f32]) -> (Vec<f32>, Vec<f32>) {
    match name {
        "eq"         => stage_eq(cfg, l, r),
        "lineq"      => stage_lineq(cfg, l, r),
        "compressor" => stage_compressor(cfg, l, r),
        "saturator"  => stage_saturator(cfg, l, r),
        "limiter"    => stage_limiter(cfg, l, r),
        "midside"    => stage_midside(cfg, l, r),
        "vocal"      => stage_vocal(cfg, l, r),
        "filter"     => stage_filter(cfg, l, r),
        "reverb"     => stage_reverb(cfg, l, r),
        "supermass"  => stage_supermass(cfg, l, r),
        "delay"      => stage_delay(cfg, l, r),
        "chorus"     => stage_chorus(cfg, l, r),
        other => panic!("unknown plugin '{other}' — supported: eq, lineq, compressor, saturator, limiter, midside, vocal, filter, reverb, supermass, delay, chorus"),
    }
}

// ===========================================================================
// Config + measurement
// ===========================================================================

#[derive(Debug, Deserialize)]
struct ChainConfig {
    stage: Vec<StageConfig>,
}

#[derive(Debug, Deserialize)]
struct StageConfig {
    plugin: String,
    #[serde(default)]
    params: toml::Table,
}

fn measure(name: &str, l: &[f32], r: &[f32]) {
    let mut meter = LoudnessMeter::new(SR as f32);
    let mut tp = TruePeakDetector::new();
    for (a, b) in l.iter().zip(r.iter()) {
        meter.process_stereo(*a, *b);
        tp.process_stereo(*a, *b);
    }
    let n = (l.len() + r.len()) as f32;
    let sum_sq: f32 = l.iter().map(|x| x * x).sum::<f32>() + r.iter().map(|x| x * x).sum::<f32>();
    let rms = (sum_sq / n).sqrt();
    let tp_db = tp.dbtp();
    let tp_disp = if tp_db.is_finite() { format!("{tp_db:>6.1}") } else { "  -inf".to_string() };
    println!(
        "  [{name:>12}]  LUFS-I {:>6.2}   TP {} dBTP   RMS {:.4}",
        meter.integrated_lufs(),
        tp_disp,
        rms,
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        eprintln!("Usage: sdsp-chain <chain.toml> <input.wav> <output.wav>");
        std::process::exit(1);
    }
    let chain_path = PathBuf::from(&args[0]);
    let in_path = PathBuf::from(&args[1]);
    let out_path = PathBuf::from(&args[2]);

    let cfg: ChainConfig =
        toml::from_str(&std::fs::read_to_string(&chain_path).expect("read chain"))
            .expect("parse chain TOML");

    println!("══ sdsp-chain ══");
    println!("Chain: {}  ({} stages)", chain_path.display(), cfg.stage.len());
    println!("Input: {}", in_path.display());
    println!();

    let wav = parse_wav_file(&in_path).expect("input WAV");
    let frames = wav.frame_count();
    let (mut cur_l, mut cur_r): (Vec<f32>, Vec<f32>) = if wav.channels >= 2 {
        let mut l = Vec::with_capacity(frames);
        let mut r = Vec::with_capacity(frames);
        for i in 0..frames {
            let (a, b) = wav.read_stereo_at(i);
            l.push(a);
            r.push(b);
        }
        (l, r)
    } else {
        (wav.samples.clone(), wav.samples.clone())
    };

    println!("Per-stage analysis (LUFS-Integrated + True-Peak):");
    measure("input", &cur_l, &cur_r);

    for (i, stage) in cfg.stage.iter().enumerate() {
        let (l, r) = dispatch(&stage.plugin, &stage.params, &cur_l, &cur_r);
        cur_l = l;
        cur_r = r;
        let label = format!("{}. {}", i + 1, stage.plugin);
        measure(&label, &cur_l, &cur_r);
    }

    let mono: Vec<f32> = cur_l.iter().zip(cur_r.iter()).map(|(a, b)| 0.5 * (a + b)).collect();
    write_mono_f32_wav(&out_path, &mono, SR as u32).expect("write output");
    println!();
    println!("→ Wrote {}", out_path.display());
}
