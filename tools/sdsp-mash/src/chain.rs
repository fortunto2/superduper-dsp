//! Master chain — statically-linked SuperDuper CLAP plugins run headless,
//! stereo in / stereo out. Lifted from `tools/sdsp-chain`'s host machinery
//! (`impl_stage!` + `dispatch`), trimmed to the plugins useful on a mashup
//! master bus and kept stereo end-to-end (sdsp-chain folds to mono at the
//! very end; a mashup master wants to stay stereo).

use clack_common::events::event_types::ParamValueEvent;
use clack_common::events::Pckn;
use clack_common::utils::{ClapId, Cookie};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_plugin::entry::SinglePluginEntry;

use crate::config::MasterStage;

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

/// Const-fn workaround for a `&'static CStr` from a string literal.
macro_rules! concat_cstr {
    ($lit:literal) => {{
        // SAFETY: literal is ASCII and we append NUL.
        unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(concat!($lit, "\0").as_bytes()) }
    }};
}

macro_rules! impl_stage {
    ($fn_name:ident, $plugin_ty:path, $bundle_id:literal, $entry_path:literal) => {
        fn $fn_name(
            params_cfg: &toml::Table,
            in_l: &[f32],
            in_r: &[f32],
            sr: f64,
        ) -> (Vec<f32>, Vec<f32>) {
            let entry = PluginEntry::load_from_clack::<SinglePluginEntry<$plugin_ty>>(
                concat_cstr!($entry_path),
            )
            .expect("entry");
            let host_info =
                HostInfo::new("sdsp-mash", "SuperDuperAI", "https://sdai.co", "0.1").unwrap();
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
                .activate(
                    |_, _| (),
                    PluginAudioConfiguration {
                        sample_rate: sr,
                        min_frames_count: BLOCK,
                        max_frames_count: BLOCK,
                    },
                )
                .expect("activate");

            let mut out_l = vec![0.0f32; n];
            let mut out_r = vec![0.0f32; n];

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
                        proc.process(
                            &input_audio,
                            &mut output_audio,
                            &inputs,
                            &mut outputs,
                            None,
                            None,
                        )
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

impl_stage!(stage_eq,         superduper_eq::SuperDuperEq,                 "co.superduperai.eq",         "/sdsp-mash/eq");
impl_stage!(stage_lineq,      superduper_lineq::SuperDuperLinEq,           "co.superduperai.lineq",      "/sdsp-mash/lineq");
impl_stage!(stage_compressor, superduper_compressor::SuperDuperCompressor, "co.superduperai.compressor", "/sdsp-mash/comp");
impl_stage!(stage_saturator,  superduper_saturator::SuperDuperSaturator,   "co.superduperai.saturator",  "/sdsp-mash/sat");
impl_stage!(stage_limiter,    superduper_limiter::SuperDuperLimiter,       "co.superduperai.limiter",    "/sdsp-mash/lim");
impl_stage!(stage_midside,    superduper_midside::SuperDuperMidSide,       "co.superduperai.midside",    "/sdsp-mash/ms");
impl_stage!(stage_filter,     superduper_filter::SuperDuperFilter,         "co.superduperai.filter",     "/sdsp-mash/flt");

const SUPPORTED: &str = "eq, lineq, compressor, saturator, limiter, midside, filter";

fn dispatch(name: &str, cfg: &toml::Table, l: &[f32], r: &[f32], sr: f64) -> (Vec<f32>, Vec<f32>) {
    match name {
        "eq" => stage_eq(cfg, l, r, sr),
        "lineq" => stage_lineq(cfg, l, r, sr),
        "compressor" => stage_compressor(cfg, l, r, sr),
        "saturator" => stage_saturator(cfg, l, r, sr),
        "limiter" => stage_limiter(cfg, l, r, sr),
        "midside" => stage_midside(cfg, l, r, sr),
        "filter" => stage_filter(cfg, l, r, sr),
        other => panic!("unknown master plugin '{other}' — supported: {SUPPORTED}"),
    }
}

/// A per-stage measurement callback: `(label, l, r)`.
pub type StageObserver<'a> = dyn FnMut(&str, &[f32], &[f32]) + 'a;

/// Run the master chain over a stereo bus. `observe` is called once per
/// stage (after it processes) so the caller can print LUFS / dBTP.
pub fn run_master(
    stages: &[MasterStage],
    mut l: Vec<f32>,
    mut r: Vec<f32>,
    sr: u32,
    observe: &mut StageObserver<'_>,
) -> (Vec<f32>, Vec<f32>) {
    for (i, st) in stages.iter().enumerate() {
        let (nl, nr) = dispatch(&st.plugin, &st.params, &l, &r, sr as f64);
        l = nl;
        r = nr;
        let label = format!("{}. {}", i + 1, st.plugin);
        observe(&label, &l, &r);
    }
    (l, r)
}
