//! sdsp-chain — headless renderer for SuperDuper plugin chains.
//!
//! Statically links the plugins (no dynamic `.clap` loading), so a render is
//! reproducible from a single TOML file and needs no DAW. This is the engine the
//! GUI app is meant to sit on top of, so everything the app will need is here:
//! multi-track mixing, per-stage sidechains, time-varying parameters, parameters
//! addressed by name, and introspection of what exists.
//!
//! ```text
//! sdsp-chain <config.toml> [<input.wav> <output.wav>]
//! sdsp-chain --list                 # every plugin the binary can render
//! sdsp-chain --params <plugin>      # that plugin's parameter table
//! ```
//!
//! # Single chain (the simple case)
//! ```toml
//! [[stage]]
//! plugin = "eq"
//! params = { Low = 1.0, High = -1.0 }     # by name, or by numeric id
//!
//! [[stage]]
//! plugin = "limiter"
//! params = { Ceiling = -1.0 }
//! ```
//!
//! # Multi-track mix (what the app needs)
//! ```toml
//! out = "render.wav"
//! tail_s = 3.0                             # let reverbs/pads ring out
//! sidechain = "voice.wav"                  # default key for every stage
//!
//! [[track]]
//! name = "kubyz"
//! input = "kubyz.wav"
//! gain_db = -2.0
//!
//!   [[track.stage]]
//!   plugin = "formant"
//!   params = { Mode = 1.0, Follow = 1.0, Glide = 22.0 }
//!   # Time-varying: [[seconds, value], …], linearly interpolated.
//!   automate = { Mix = [[0.0, 0.0], [4.0, 0.95]] }
//!
//! [[track]]
//! name = "voice pad"
//! input = "voice.wav"
//! gain_db = -6.0
//! gain_automate = [[0.0, -60.0], [6.0, -6.0]]
//!
//!   [[track.stage]]
//!   plugin = "stretch"
//!   params = { Stretch = 14.0, Window = 3.0, Tonal = 0.22 }
//!
//! [[master]]
//! plugin = "limiter"
//! params = { Ceiling = -1.0 }
//! ```
//!
//! Parameter values are in each parameter's own units (Hz, dB, ms, semitones) —
//! the same numbers the plugin GUI shows, not normalised 0..1. Run
//! `--params <plugin>` to see them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
use serde::Deserialize;

use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_synth_core::loudness::{LoudnessMeter, TruePeakDetector};
use superduper_synth_core::wav::{parse_wav_file, write_stereo_f32_wav};

/// Processing block size. Plugins are activated with this as both min and max
/// frames, so a render is bit-identical regardless of host buffer settings.
const BLOCK: u32 = 256;

// ===========================================================================
// Host boilerplate
// ===========================================================================

struct TS;
impl SharedHandler<'_> for TS {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    /// Plugins ask for this when a Preset param moves; the recall itself runs in
    /// the plugin's own `flush`, which clack calls for us.
    fn request_callback(&self) {}
}
impl HostLogImpl for TS {
    fn log(&self, severity: LogSeverity, msg: &str) {
        // Plugin-side warnings are worth seeing in a render log.
        if matches!(severity, LogSeverity::Error | LogSeverity::Warning) {
            eprintln!("  [plugin {severity}] {msg}");
        }
    }
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
// Audio buffers
// ===========================================================================

/// A stereo signal at a known sample rate.
#[derive(Clone)]
struct Audio {
    l: Vec<f32>,
    r: Vec<f32>,
    sr: f64,
}

impl Audio {
    fn silence(frames: usize, sr: f64) -> Audio {
        Audio { l: vec![0.0; frames], r: vec![0.0; frames], sr }
    }

    fn load(path: &Path) -> Result<Audio, String> {
        let w = parse_wav_file(path).map_err(|e| format!("{}: {e:?}", path.display()))?;
        let frames = w.frame_count();
        let mut l = Vec::with_capacity(frames);
        let mut r = Vec::with_capacity(frames);
        for i in 0..frames {
            let (a, b) = w.read_stereo_at(i);
            l.push(a);
            r.push(b);
        }
        Ok(Audio { l, r, sr: w.sample_rate as f64 })
    }

    fn frames(&self) -> usize {
        self.l.len().min(self.r.len())
    }

    /// Zero-pad (or truncate) to `frames`.
    fn resize(&mut self, frames: usize) {
        self.l.resize(frames, 0.0);
        self.r.resize(frames, 0.0);
    }

    /// Sum `src` in at a per-sample gain, with an equal-power pan.
    ///
    /// `pan` is −1 (hard left) … +1 (hard right); the constant-power law keeps
    /// perceived loudness steady across the sweep, which a plain linear crossfade
    /// does not — a part panned to 0.5 would otherwise drop in level.
    fn mix_from(&mut self, src: &Audio, gain: &[f32], pan: f32) {
        let theta = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
        let (gl, gr) = (theta.cos() * std::f32::consts::SQRT_2, theta.sin() * std::f32::consts::SQRT_2);
        for i in 0..self.frames().min(src.frames()) {
            // A constant gain is stored as a one-element curve, and automation
            // ends before the tail does — in both cases the right value is the
            // last one, NOT 1.0. Falling back to unity here silently ignored
            // every static `gain_db` in every mix.
            let g = gain.get(i).or_else(|| gain.last()).copied().unwrap_or(1.0);
            self.l[i] += src.l[i] * g * gl;
            self.r[i] += src.r[i] * g * gr;
        }
    }
}

// ===========================================================================
// Plugin registry — one row per renderable plugin. `--list`, `--params` and the
// render dispatch all read this same table, so they can't disagree.
// ===========================================================================

/// Resolved parameter settings for one stage: (param id, value).
type ParamSet = Vec<(u32, f64)>;
/// Resolved automation: (param id, breakpoints as (seconds, value)).
type AutoSet = Vec<(u32, Vec<(f32, f64)>)>;

struct StageIo<'a> {
    input: &'a Audio,
    sidechain: Option<&'a Audio>,
}

type RenderFn = fn(&ParamSet, &AutoSet, StageIo<'_>) -> Audio;

struct PluginSpec {
    key: &'static str,
    params: &'static [ParamDef],
    /// Factory preset names, in index order — what a `Preset` param addresses.
    presets: fn() -> Vec<&'static str>,
    render: RenderFn,
    /// Whether input port 1 exists (so we can warn when a sidechain is wasted).
    sidechain: bool,
}

macro_rules! impl_stage {
    ($fn_name:ident, $plugin_ty:path, $bundle_id:literal, $entry_path:literal) => {
        fn $fn_name(params: &ParamSet, auto: &AutoSet, io: StageIo<'_>) -> Audio {
            let entry = PluginEntry::load_from_clack::<SinglePluginEntry<$plugin_ty>>(
                concat_cstr!($entry_path),
            )
            .expect("plugin entry");
            let host_info =
                HostInfo::new("sdsp-chain", "SuperDuperAI", "https://superduperai.co", "0.1")
                    .unwrap();
            let mut plugin = PluginInstance::<TH>::new(
                |_| TS,
                |_| (),
                &entry,
                concat_cstr!($bundle_id),
                &host_info,
            )
            .expect("instantiate");

            let sr = io.input.sr;
            let frames = io.input.frames();
            let block = BLOCK as usize;
            // Round UP: dropping the final partial block would silently shorten
            // every render by up to BLOCK-1 samples.
            let n_blocks = (frames + block - 1) / block;
            let padded = n_blocks * block;

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

            let mut in_l = io.input.l.clone();
            let mut in_r = io.input.r.clone();
            in_l.resize(padded, 0.0);
            in_r.resize(padded, 0.0);
            let (sc_l, sc_r) = match io.sidechain {
                Some(sc) => {
                    let mut a = sc.l.clone();
                    let mut b = sc.r.clone();
                    a.resize(padded, 0.0);
                    b.resize(padded, 0.0);
                    (a, b)
                }
                None => (Vec::new(), Vec::new()),
            };
            let has_sc = io.sidechain.is_some();

            let mut out_l = vec![0.0f32; padded];
            let mut out_r = vec![0.0f32; padded];

            // Initial param values, applied on the first block.
            let mut init_events = EventBuffer::new();
            for (id, v) in params.iter() {
                init_events.push(&ParamValueEvent::new(
                    0,
                    ClapId::new(*id),
                    Pckn::new(0u16, 0u16, 0u16, 0u32),
                    *v,
                    Cookie::empty(),
                ));
            }

            let in_l_ref = &in_l;
            let in_r_ref = &in_r;
            let sc_l_ref = &sc_l;
            let sc_r_ref = &sc_r;
            let out_l_ref = &mut out_l;
            let out_r_ref = &mut out_r;
            let events_ref = &init_events;
            let auto_ref = auto;

            let stopped_back = std::thread::scope(|s| {
                s.spawn(move || {
                    let mut proc = stopped.start_processing().expect("start_processing");
                    let mut in_ports = AudioPorts::with_capacity(2, 2);
                    let mut out_ports = AudioPorts::with_capacity(2, 1);

                    for blk in 0..n_blocks {
                        let start = blk * block;
                        let end = start + block;

                        let mut chunk_l = in_l_ref[start..end].to_vec();
                        let mut chunk_r = in_r_ref[start..end].to_vec();
                        let mut key_l = if has_sc { sc_l_ref[start..end].to_vec() } else { Vec::new() };
                        let mut key_r = if has_sc { sc_r_ref[start..end].to_vec() } else { Vec::new() };

                        let mut bufs = Vec::with_capacity(2);
                        bufs.push(AudioPortBuffer {
                            latency: 0,
                            channels: AudioPortBufferType::f32_input_only(
                                [
                                    InputChannel::variable(chunk_l.as_mut_slice()),
                                    InputChannel::variable(chunk_r.as_mut_slice()),
                                ]
                                .into_iter(),
                            ),
                        });
                        if has_sc {
                            bufs.push(AudioPortBuffer {
                                latency: 0,
                                channels: AudioPortBufferType::f32_input_only(
                                    [
                                        InputChannel::variable(key_l.as_mut_slice()),
                                        InputChannel::variable(key_r.as_mut_slice()),
                                    ]
                                    .into_iter(),
                                ),
                            });
                        }
                        let input_audio = in_ports.with_input_buffers(bufs);

                        let l_buf = &mut out_l_ref[start..end];
                        let r_buf = &mut out_r_ref[start..end];
                        let mut out_chans: [&mut [f32]; 2] = [l_buf, r_buf];
                        let mut output_audio = out_ports.with_output_buffers([AudioPortBuffer {
                            latency: 0,
                            channels: AudioPortBufferType::f32_output_only(
                                out_chans.iter_mut().map(|b| &mut **b),
                            ),
                        }]);

                        // One automation event per automated param per block. A
                        // block is 5.3 ms at 48 kHz — finer than a DAW's own
                        // automation grid, and enough for Freeze to land on the
                        // beat you asked for.
                        let mut blk_events = EventBuffer::new();
                        if blk == 0 {
                            for ev in events_ref.iter() {
                                blk_events.push(ev);
                            }
                        }
                        let t = start as f32 / sr as f32;
                        for (id, points) in auto_ref.iter() {
                            blk_events.push(&ParamValueEvent::new(
                                0,
                                ClapId::new(*id),
                                Pckn::new(0u16, 0u16, 0u16, 0u32),
                                interp_at(points, t),
                                Cookie::empty(),
                            ));
                        }

                        let inputs = InputEvents::from_buffer(&blk_events);
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

            out_l.truncate(frames);
            out_r.truncate(frames);
            Audio { l: out_l, r: out_r, sr }
        }
    };
}

/// `&'static CStr` from a literal — the `c"…"` macro's lifetime doesn't satisfy
/// the clack-host APIs.
macro_rules! concat_cstr {
    ($lit:literal) => {{
        // SAFETY: the literal is ASCII and we append the NUL ourselves.
        unsafe { std::ffi::CStr::from_bytes_with_nul_unchecked(concat!($lit, "\0").as_bytes()) }
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
impl_stage!(stage_formant,    superduper_formant::SuperDuperFormant,       "co.superduperai.formant",    "/sdsp-chain/fmt");
impl_stage!(stage_granular,   superduper_granular::SuperDuperGranular,     "co.superduperai.granular",   "/sdsp-chain/gran");
impl_stage!(stage_stretch,    superduper_stretch::SuperDuperStretch,       "co.superduperai.stretch",    "/sdsp-chain/str");
impl_stage!(stage_pitch,      superduper_pitch::SuperDuperPitch,           "co.superduperai.pitch",      "/sdsp-chain/pit");
impl_stage!(stage_tune,       superduper_tune::SuperDuperTune,             "co.superduperai.tune",       "/sdsp-chain/tun");
impl_stage!(stage_vocoder,    superduper_vocoder::SuperDuperVocoder,       "co.superduperai.vocoder",    "/sdsp-chain/voc2");
impl_stage!(stage_harmonic,   superduper_harmonic::SuperDuperHarmonic,     "co.superduperai.harmonic",   "/sdsp-chain/harm");
impl_stage!(stage_wind,       superduper_wind::SuperDuperWind,             "co.superduperai.wind",       "/sdsp-chain/wind");
impl_stage!(stage_soothe,     superduper_soothe::SuperDuperSoothe,         "co.superduperai.soothe",     "/sdsp-chain/sth");

fn registry() -> &'static [PluginSpec] {
    &[
        PluginSpec { key: "eq",         params: superduper_eq::PARAMS,         presets: || superduper_eq::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_eq,         sidechain: false },
        PluginSpec { key: "lineq",      params: superduper_lineq::PARAMS,      presets: || superduper_lineq::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_lineq,      sidechain: false },
        PluginSpec { key: "compressor", params: superduper_compressor::PARAMS, presets: || superduper_compressor::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_compressor, sidechain: true  },
        PluginSpec { key: "saturator",  params: superduper_saturator::PARAMS,  presets: || superduper_saturator::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_saturator,  sidechain: false },
        PluginSpec { key: "limiter",    params: superduper_limiter::PARAMS,    presets: || superduper_limiter::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_limiter,    sidechain: false },
        PluginSpec { key: "midside",    params: superduper_midside::PARAMS,    presets: || superduper_midside::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_midside,    sidechain: false },
        PluginSpec { key: "vocal",      params: superduper_vocal::PARAMS,      presets: || superduper_vocal::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_vocal,      sidechain: false },
        PluginSpec { key: "filter",     params: superduper_filter::PARAMS,     presets: || superduper_filter::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_filter,     sidechain: false },
        PluginSpec { key: "reverb",     params: superduper_reverb::PARAMS,     presets: || superduper_reverb::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_reverb,     sidechain: true  },
        PluginSpec { key: "supermass",  params: superduper_supermass::PARAMS,  presets: || superduper_supermass::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_supermass,  sidechain: true  },
        PluginSpec { key: "delay",      params: superduper_delay::PARAMS,      presets: || superduper_delay::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_delay,      sidechain: true  },
        PluginSpec { key: "chorus",     params: superduper_chorus::PARAMS,     presets: || superduper_chorus::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_chorus,     sidechain: false },
        PluginSpec { key: "formant",    params: superduper_formant::PARAMS,    presets: || superduper_formant::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_formant,    sidechain: true  },
        PluginSpec { key: "granular",   params: superduper_granular::PARAMS,   presets: || superduper_granular::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_granular,   sidechain: false },
        PluginSpec { key: "stretch",    params: superduper_stretch::PARAMS,    presets: || superduper_stretch::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_stretch,    sidechain: false },
        // Pitch/Tune matter here because two takes can only be merged musically
        // once they share a fundamental; Vocoder's sidechain is its Carrier.
        PluginSpec { key: "pitch",      params: superduper_pitch::PARAMS,      presets: || superduper_pitch::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_pitch,      sidechain: false },
        PluginSpec { key: "tune",       params: superduper_tune::PARAMS,       presets: || superduper_tune::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_tune,       sidechain: true  },
        PluginSpec { key: "vocoder",    params: superduper_vocoder::PARAMS,    presets: || superduper_vocoder::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_vocoder,    sidechain: true  },
        PluginSpec { key: "harmonic",   params: superduper_harmonic::PARAMS,   presets: || superduper_harmonic::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_harmonic,   sidechain: false },
        PluginSpec { key: "wind",       params: superduper_wind::PARAMS,       presets: || superduper_wind::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_wind,       sidechain: false },
        PluginSpec { key: "soothe",     params: superduper_soothe::PARAMS,     presets: || superduper_soothe::presets::PRESETS.iter().map(|p| p.name).collect(), render: stage_soothe,     sidechain: false },
    ]
}

/// Instruments can't be rendered by the chain (no MIDI here), but their tables
/// are exactly what you need when setting their parameters from a DAW over MCP —
/// so introspection covers them too, and `--list` says which is which.
struct InstrumentSpec {
    key: &'static str,
    params: &'static [ParamDef],
    presets: fn() -> Vec<&'static str>,
}

fn instruments() -> Vec<InstrumentSpec> {
    vec![
        InstrumentSpec { key: "wave",  params: superduper_wave::PARAMS,
                         presets: || superduper_wave::presets::PRESETS.iter().map(|p| p.name).collect() },
        InstrumentSpec { key: "kubyz", params: superduper_kubyz::PARAMS,
                         presets: || superduper_kubyz::presets::presets().iter().map(|p| p.name).collect() },
        InstrumentSpec { key: "pad",   params: superduper_pad::PARAMS,
                         presets: || superduper_pad::presets::PRESETS.iter().map(|p| p.name).collect() },
        InstrumentSpec { key: "drum",  params: superduper_drum::PARAMS,
                         presets: || superduper_drum::presets::PRESETS.iter().map(|p| p.name).collect() },
    ]
}

/// Params + presets for anything we know about, effect or instrument.
fn tables_for(key: &str) -> Result<(&'static [ParamDef], Vec<&'static str>), String> {
    if let Some(s) = registry().iter().find(|s| s.key == key) {
        return Ok((s.params, (s.presets)()));
    }
    if let Some(i) = instruments().into_iter().find(|i| i.key == key) {
        return Ok((i.params, (i.presets)()));
    }
    let mut keys: Vec<&str> = registry().iter().map(|s| s.key).collect();
    keys.extend(instruments().iter().map(|i| i.key));
    Err(format!("unknown plugin '{key}'. Available: {}", keys.join(", ")))
}

fn spec_for(key: &str) -> Result<&'static PluginSpec, String> {
    registry().iter().find(|s| s.key == key).ok_or_else(|| {
        let keys: Vec<&str> = registry().iter().map(|s| s.key).collect();
        format!("unknown plugin '{key}'. Available: {}", keys.join(", "))
    })
}

fn param_name(d: &ParamDef) -> &str {
    std::str::from_utf8(d.name).unwrap_or("?")
}

/// Resolve a config key to a param id: either the parameter's name
/// (case-insensitive) or its numeric id. An unknown key is an error rather than
/// a silent no-op — a typo'd knob that quietly does nothing is worse than a
/// failed render.
fn resolve_param(spec: &PluginSpec, key: &str) -> Result<u32, String> {
    if let Some(d) = spec
        .params
        .iter()
        .find(|d| param_name(d).eq_ignore_ascii_case(key))
    {
        return Ok(d.id);
    }
    if let Ok(id) = key.parse::<u32>() {
        if spec.params.iter().any(|d| d.id == id) {
            return Ok(id);
        }
        return Err(format!(
            "'{}' has no param id {id} (it has {})",
            spec.key,
            spec.params.len()
        ));
    }
    let names: Vec<&str> = spec.params.iter().map(param_name).collect();
    Err(format!(
        "'{}' has no param named '{key}'. Params: {}",
        spec.key,
        names.join(", ")
    ))
}

/// Warn when a value sits outside the parameter's declared range — the plugin
/// will clamp it, and a silently clamped value looks like the config was ignored.
fn check_range(spec: &PluginSpec, id: u32, v: f64) {
    if let Some(d) = spec.params.iter().find(|d| d.id == id) {
        if v < d.min || v > d.max {
            eprintln!(
                "  warning: {}.{} = {v} is outside {}..{} and will be clamped",
                spec.key,
                param_name(d),
                d.min,
                d.max
            );
        }
    }
}

/// Value of a breakpoint list at time `t` (seconds), linearly interpolated,
/// clamped at both ends.
fn interp_at(points: &[(f32, f64)], t: f32) -> f64 {
    match points.first() {
        None => 0.0,
        Some(&(t0, v0)) if t <= t0 => v0,
        _ => {
            let last = points[points.len() - 1];
            if t >= last.0 {
                return last.1;
            }
            for w in points.windows(2) {
                let (ta, va) = w[0];
                let (tb, vb) = w[1];
                if t >= ta && t <= tb {
                    let f = if (tb - ta).abs() < 1e-9 {
                        0.0
                    } else {
                        ((t - ta) / (tb - ta)) as f64
                    };
                    return va + (vb - va) * f;
                }
            }
            last.1
        }
    }
}

// ===========================================================================
// Config
// ===========================================================================

#[derive(Debug, Deserialize)]
struct Config {
    /// Output path; the CLI argument wins if both are given.
    #[serde(default)]
    out: Option<String>,
    /// Extra silence appended to every input so reverbs, pads and frozen clouds
    /// ring out instead of being cut at the last input sample.
    #[serde(default)]
    tail_s: f64,
    /// Default sidechain key for every sidechain-capable stage.
    #[serde(default)]
    sidechain: Option<String>,
    /// Single-chain form: stages applied to the CLI input.
    #[serde(default)]
    stage: Vec<StageCfg>,
    /// Multi-track form: each track has its own input and chain, then they're
    /// summed.
    #[serde(default)]
    track: Vec<TrackCfg>,
    /// Stages applied to the summed mix.
    #[serde(default)]
    master: Vec<StageCfg>,
}

#[derive(Debug, Deserialize)]
struct TrackCfg {
    /// Duck this track off another one — `duck_from = "kick"`.
    ///
    /// Appends a compressor at the end of the chain keyed off that track's
    /// finished render. This is the single most load-bearing move in a mix
    /// with both a kick and a bass: they share 40-120 Hz, they sum, and the
    /// limiter eats the result. Ducking the bass 3-6 dB for ~90 ms gives the
    /// kick its own moment and hands back the headroom.
    duck_from: Option<String>,
    /// How hard to duck, in dB of gain reduction. Default 5.
    duck_db: Option<f64>,
    /// How long to stay down, in ms. Default 90 — recovered before the next
    /// beat at anything from 100 to 150 BPM.
    duck_release_ms: Option<f64>,
    #[serde(default)]
    name: Option<String>,
    /// WAV for this track; omitted means the CLI input.
    #[serde(default)]
    input: Option<String>,
    #[serde(default)]
    sidechain: Option<String>,
    #[serde(default)]
    gain_db: Option<f64>,
    /// Stereo placement, −1 (left) … +1 (right). Equal-power, so panning a part
    /// off-centre doesn't change how loud it seems.
    #[serde(default)]
    pan: Option<f64>,
    /// Time-varying track gain in dB: `[[seconds, dB], …]`.
    #[serde(default)]
    gain_automate: Vec<[f64; 2]>,
    #[serde(default)]
    mute: bool,
    #[serde(default)]
    stage: Vec<StageCfg>,
}

#[derive(Debug, Deserialize)]
struct StageCfg {
    plugin: String,
    #[serde(default)]
    params: toml::Table,
    /// `automate = { Mix = [[0.0, 0.0], [4.0, 1.0]] }`
    #[serde(default)]
    automate: toml::Table,
    /// Per-stage sidechain. `""` means "no sidechain for this stage".
    #[serde(default)]
    sidechain: Option<String>,
}

/// A stage with everything resolved against the plugin's param table.
struct ResolvedStage {
    spec: &'static PluginSpec,
    params: ParamSet,
    auto: AutoSet,
    sidechain: Option<String>,
}

fn resolve_stage(cfg: &StageCfg) -> Result<ResolvedStage, String> {
    let spec = spec_for(&cfg.plugin)?;
    let mut params = ParamSet::new();
    for (key, val) in cfg.params.iter() {
        let v = val
            .as_float()
            .or_else(|| val.as_integer().map(|i| i as f64))
            .or_else(|| val.as_bool().map(|b| if b { 1.0 } else { 0.0 }))
            .ok_or_else(|| format!("{}.{key}: expected a number", cfg.plugin))?;
        let id = resolve_param(spec, key)?;
        check_range(spec, id, v);
        params.push((id, v));
    }
    let mut auto = AutoSet::new();
    for (key, val) in cfg.automate.iter() {
        let id = resolve_param(spec, key)?;
        let list = val
            .as_array()
            .ok_or_else(|| format!("{}.automate.{key}: expected [[seconds, value], …]", cfg.plugin))?;
        let mut points = Vec::new();
        for item in list {
            let pair = item.as_array().ok_or_else(|| {
                format!("{}.automate.{key}: each entry must be [seconds, value]", cfg.plugin)
            })?;
            if pair.len() != 2 {
                return Err(format!(
                    "{}.automate.{key}: each entry must be [seconds, value]",
                    cfg.plugin
                ));
            }
            let num = |x: &toml::Value| {
                x.as_float().or_else(|| x.as_integer().map(|i| i as f64))
            };
            let (t, v) = (num(&pair[0]), num(&pair[1]));
            match (t, v) {
                (Some(t), Some(v)) => {
                    check_range(spec, id, v);
                    points.push((t as f32, v));
                }
                _ => {
                    return Err(format!(
                        "{}.automate.{key}: seconds and value must be numbers",
                        cfg.plugin
                    ))
                }
            }
        }
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        if !points.is_empty() {
            auto.push((id, points));
        }
    }
    Ok(ResolvedStage { spec, params, auto, sidechain: cfg.sidechain.clone() })
}

// ===========================================================================
// Measurement
// ===========================================================================

fn measure(label: &str, a: &Audio) {
    let mut meter = LoudnessMeter::new(a.sr as f32);
    let mut tp = TruePeakDetector::new();
    for (l, r) in a.l.iter().zip(a.r.iter()) {
        meter.process_stereo(*l, *r);
        tp.process_stereo(*l, *r);
    }
    let n = (a.l.len() + a.r.len()).max(1) as f32;
    let sum_sq: f32 =
        a.l.iter().map(|x| x * x).sum::<f32>() + a.r.iter().map(|x| x * x).sum::<f32>();
    let rms = (sum_sq / n).sqrt();
    let tp_db = tp.dbtp();
    let tp_disp = if tp_db.is_finite() { format!("{tp_db:>6.1}") } else { "  -inf".into() };
    println!(
        "  [{label:>22}]  LUFS-I {:>7.2}   TP {tp_disp} dBTP   RMS {rms:.4}",
        meter.integrated_lufs()
    );
}

// ===========================================================================
// Rendering
// ===========================================================================

/// Load every distinct sidechain path once — a chain often keys several stages
/// off the same voice take.
///
/// A key can also be another track in the same config, written `track:<name>`.
/// That is what makes ducking possible headlessly: the kick's finished render
/// becomes the key that pulls the bass down, with no DAW routing involved.
struct SidechainCache {
    files: BTreeMap<String, Audio>,
    /// Tracks already rendered this run, by name, for `track:` keys.
    tracks: BTreeMap<String, Audio>,
}

impl SidechainCache {
    fn new() -> Self {
        Self { files: BTreeMap::new(), tracks: BTreeMap::new() }
    }
    /// Publish a finished track so later tracks can key off it.
    fn publish(&mut self, name: &str, audio: &Audio) {
        self.tracks.insert(name.to_string(), audio.clone());
    }
    fn get(&mut self, path: &str, frames: usize, sr: f64) -> Result<&Audio, String> {
        if let Some(name) = path.strip_prefix("track:") {
            return self.tracks.get(name).ok_or_else(|| {
                format!(
                    "sidechain 'track:{name}' — no track called '{name}' has been rendered yet. \
                     Tracks render in config order, so the key has to be declared above the \
                     track that ducks off it."
                )
            });
        }
        if !self.files.contains_key(path) {
            let mut a = Audio::load(Path::new(path))?;
            if (a.sr - sr).abs() > 1.0 {
                return Err(format!(
                    "sidechain {path} is {} Hz but the render is {sr} Hz — resample it first",
                    a.sr
                ));
            }
            a.resize(frames);
            self.files.insert(path.to_string(), a);
        }
        Ok(self.files.get(path).unwrap())
    }
}

fn run_chain(
    label: &str,
    mut cur: Audio,
    stages: &[ResolvedStage],
    default_sc: Option<&str>,
    sc_cache: &mut SidechainCache,
) -> Result<Audio, String> {
    for (i, st) in stages.iter().enumerate() {
        // Per-stage sidechain wins; "" disables; otherwise the chain default.
        let sc_path = match st.sidechain.as_deref() {
            Some("") => None,
            Some(p) => Some(p),
            None => default_sc,
        };
        let sc = match sc_path {
            Some(p) if st.spec.sidechain => Some(sc_cache.get(p, cur.frames(), cur.sr)?.clone()),
            Some(p) => {
                // Only worth reporting when the user named it on THIS stage; a
                // chain-level default is meant to land on every stage that can
                // use one and be ignored by the rest.
                if st.sidechain.as_deref().is_some_and(|x| !x.is_empty()) {
                    eprintln!(
                        "  note: {} has no sidechain input — ignoring '{p}' for this stage",
                        st.spec.key
                    );
                }
                None
            }
            None => None,
        };
        let io = StageIo { input: &cur, sidechain: sc.as_ref() };
        cur = (st.spec.render)(&st.params, &st.auto, io);
        let mut tag = format!("{label}{}. {}", i + 1, st.spec.key);
        if sc.is_some() {
            tag.push_str(" +sc");
        }
        measure(&tag, &cur);
    }
    Ok(cur)
}

/// dB breakpoints → a per-sample linear gain curve. With no breakpoints this is
/// a single-element curve holding the static gain; `mix_from` reads past the end
/// by holding the last value, so one element behaves as a constant.
fn gain_curve(points: &[[f64; 2]], base_db: f64, frames: usize, sr: f64) -> Vec<f32> {
    if points.is_empty() {
        return vec![10f64.powf(base_db / 20.0) as f32; 1];
    }
    let pts: Vec<(f32, f64)> = points.iter().map(|p| (p[0] as f32, p[1])).collect();
    (0..frames)
        .map(|i| {
            let db = interp_at(&pts, i as f32 / sr as f32);
            10f64.powf(db / 20.0) as f32
        })
        .collect()
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("sdsp-chain: {e}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(|s| s.as_str()) {
        None | Some("-h") | Some("--help") => {
            println!("Usage:");
            println!("  sdsp-chain <config.toml> [<input.wav> <output.wav>]");
            println!("  sdsp-chain --list");
            println!("  sdsp-chain --params <plugin>");
            println!("  sdsp-chain --presets <plugin>");
            return Ok(());
        }
        Some("--list") => {
            println!("Renderable effects ({}):", registry().len());
            for s in registry() {
                println!(
                    "  {:<11} {:>2} params{}",
                    s.key,
                    s.params.len(),
                    if s.sidechain { "   (has sidechain input)" } else { "" }
                );
            }
            println!("\nInstruments (introspection only — the chain has no MIDI):");
            for i in instruments() {
                println!("  {:<11} {:>2} params, {} presets", i.key, i.params.len(), (i.presets)().len());
            }
            return Ok(());
        }
        Some("--presets") => {
            let key = args.get(1).ok_or("--presets needs a plugin key")?;
            let (_, names) = tables_for(key)?;
            if names.is_empty() {
                println!("{key} has no factory presets");
                return Ok(());
            }
            println!("{key} — {} presets. Set the `Preset` param to the INDEX (a real", names.len());
            println!("value, not 0..1) — over MCP that is track_fx_set_param(track, fx, <Preset id>, index).");
            for (i, n) in names.iter().enumerate() {
                println!("  {i:<3} {n}");
            }
            return Ok(());
        }
        Some("--params") => {
            let key = args.get(1).ok_or("--params needs a plugin key")?;
            let (params, presets) = tables_for(key)?;
            println!("{key} — {} params", params.len());
            println!("  {:<4} {:<12} {:>10} {:>10} {:>10}  unit", "id", "name", "min", "max", "default");
            for d in params {
                println!(
                    "  {:<4} {:<12} {:>10.3} {:>10.3} {:>10.3}  {}",
                    d.id,
                    param_name(d),
                    d.min,
                    d.max,
                    d.default,
                    d.unit
                );
            }
            if !presets.is_empty() {
                println!("\n  {} presets — see `--presets {key}`", presets.len());
            }
            return Ok(());
        }
        _ => {}
    }

    let config_path = PathBuf::from(&args[0]);
    let cli_input = args.get(1).map(PathBuf::from);
    let cli_output = args.get(2).map(PathBuf::from);

    let cfg: Config = toml::from_str(
        &std::fs::read_to_string(&config_path)
            .map_err(|e| format!("read {}: {e}", config_path.display()))?,
    )
    .map_err(|e| format!("parse {}: {e}", config_path.display()))?;

    let out_path = cli_output
        .or_else(|| cfg.out.as_ref().map(PathBuf::from))
        .ok_or("no output path — pass one on the command line or set `out` in the config")?;

    println!("══ sdsp-chain ══");
    println!("Config: {}", config_path.display());

    // ---- Assemble the track list ------------------------------------------
    struct Track {
        name: String,
        audio: Audio,
        stages: Vec<ResolvedStage>,
        sidechain: Option<String>,
        gain_db: f64,
        pan: f32,
        gain_automate: Vec<[f64; 2]>,
        mute: bool,
    }

    let load_input = |p: &Path| -> Result<Audio, String> { Audio::load(p) };

    let mut tracks: Vec<Track> = Vec::new();
    if cfg.track.is_empty() {
        // Single-chain form.
        let input = cli_input
            .clone()
            .ok_or("no input WAV — pass one on the command line or use [[track]] entries")?;
        let mut stages = Vec::new();
        for st in &cfg.stage {
            stages.push(resolve_stage(st)?);
        }
        tracks.push(Track {
            name: "input".into(),
            audio: load_input(&input)?,
            stages,
            sidechain: None,
            gain_db: 0.0,
            pan: 0.0,
            gain_automate: Vec::new(),
            mute: false,
        });
    } else {
        for (i, t) in cfg.track.iter().enumerate() {
            let path = match (&t.input, &cli_input) {
                (Some(p), _) => PathBuf::from(p),
                (None, Some(p)) => p.clone(),
                (None, None) => {
                    return Err(format!(
                        "track {} has no `input` and no CLI input was given",
                        t.name.clone().unwrap_or_else(|| (i + 1).to_string())
                    ))
                }
            };
            let mut stages = Vec::new();
            for st in &t.stage {
                stages.push(resolve_stage(st)?);
            }
            if let Some(key) = &t.duck_from {
                // Fast attack so the duck lands on the transient, no lookahead
                // (ducking early sounds like a mistake), and the sidechain HPF
                // off — the key here IS low end, filtering it would deafen the
                // detector. Ratio and threshold are derived from the requested
                // depth so the knob the user turns is "how many dB".
                let depth = t.duck_db.unwrap_or(5.0).clamp(1.0, 24.0);
                let release = t.duck_release_ms.unwrap_or(90.0).clamp(5.0, 1000.0);
                let mut params: BTreeMap<String, f64> = BTreeMap::new();
                params.insert("Threshold".into(), -26.0);
                params.insert("Ratio".into(), (1.0 + depth * 1.0).min(20.0));
                params.insert("Attack".into(), 0.5);
                params.insert("Release".into(), release);
                params.insert("Knee".into(), 3.0);
                params.insert("SC HPF".into(), 0.0);
                params.insert("Auto Rel".into(), 0.0);
                params.insert("Lookahead".into(), 0.0);
                params.insert("Mix".into(), 1.0);
                let cfg_stage = StageCfg {
                    plugin: "compressor".into(),
                    params: params
                        .into_iter()
                        .map(|(k, v)| (k, toml::Value::Float(v)))
                        .collect(),
                    automate: toml::Table::new(),
                    sidechain: Some(format!("track:{key}")),
                };
                stages.push(resolve_stage(&cfg_stage)?);
            }
            tracks.push(Track {
                name: t.name.clone().unwrap_or_else(|| format!("track {}", i + 1)),
                audio: load_input(&path)?,
                stages,
                sidechain: t.sidechain.clone(),
                gain_db: t.gain_db.unwrap_or(0.0),
                pan: t.pan.unwrap_or(0.0) as f32,
                gain_automate: t.gain_automate.clone(),
                mute: t.mute,
            });
        }
    }

    // ---- Sample rate + length ---------------------------------------------
    let sr = tracks[0].audio.sr;
    for t in &tracks {
        if (t.audio.sr - sr).abs() > 1.0 {
            return Err(format!(
                "track '{}' is {} Hz but '{}' is {sr} Hz — all inputs must share a rate",
                t.name, t.audio.sr, tracks[0].name
            ));
        }
    }
    let tail = (cfg.tail_s.max(0.0) * sr) as usize;
    let frames = tracks.iter().map(|t| t.audio.frames()).max().unwrap_or(0) + tail;
    for t in tracks.iter_mut() {
        t.audio.resize(frames);
    }
    println!(
        "Rate: {sr:.0} Hz   Length: {:.2} s ({} tracks, {} master stages)",
        frames as f64 / sr,
        tracks.len(),
        cfg.master.len()
    );
    if tail > 0 {
        println!("Tail: {:.2} s of silence appended", cfg.tail_s);
    }
    if let Some(sc) = cfg.sidechain.as_deref() {
        println!("Default sidechain: {sc}");
    }
    println!();

    let mut sc_cache = SidechainCache::new();
    let mut master = Audio::silence(frames, sr);

    for t in tracks.iter() {
        // A muted track still renders, because something later may key off it:
        // "kick used only as a sidechain source, never heard" is a normal way
        // to build a mix. It just doesn't reach the master.
        if t.mute {
            println!("── {} (muted — rendered as a key only)", t.name);
            let keyed = run_chain(
                "  ",
                t.audio.clone(),
                &t.stages,
                t.sidechain.as_deref().or(cfg.sidechain.as_deref()).filter(|p| !p.is_empty()),
                &mut sc_cache,
            )?;
            sc_cache.publish(&t.name, &keyed);
            continue;
        }
        println!("── {}", t.name);
        measure("in", &t.audio);
        let default_sc = t.sidechain.as_deref().or(cfg.sidechain.as_deref());
        let rendered = run_chain(
            "  ",
            t.audio.clone(),
            &t.stages,
            default_sc.filter(|p| !p.is_empty()),
            &mut sc_cache,
        )?;
        // Publish before gain/pan: a key should follow the part's sound, not
        // the fader move the engineer makes afterwards.
        sc_cache.publish(&t.name, &rendered);
        let curve = gain_curve(&t.gain_automate, t.gain_db, frames, sr);
        if !t.gain_automate.is_empty() {
            let lo = t.gain_automate.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min);
            let hi = t.gain_automate.iter().map(|p| p[1]).fold(f64::NEG_INFINITY, f64::max);
            println!("  gain automated {lo:+.1} … {hi:+.1} dB over {} points", t.gain_automate.len());
        } else if t.gain_db != 0.0 {
            println!("  gain {:+.1} dB", t.gain_db);
        }
        if t.pan.abs() > 0.001 {
            println!("  pan {:+.2} ({})", t.pan, if t.pan < 0.0 { "left" } else { "right" });
        }
        master.mix_from(&rendered, &curve, t.pan);
    }

    if tracks.len() > 1 || !cfg.master.is_empty() {
        println!("── master");
        measure("mix", &master);
    }
    let mut master_stages = Vec::new();
    for st in &cfg.master {
        master_stages.push(resolve_stage(st)?);
    }
    let final_mix = run_chain(
        "  ",
        master,
        &master_stages,
        cfg.sidechain.as_deref().filter(|p| !p.is_empty()),
        &mut sc_cache,
    )?;

    write_stereo_f32_wav(&out_path, &final_mix.l, &final_mix.r, sr as u32)
        .map_err(|e| format!("write {}: {e:?}", out_path.display()))?;
    println!("\nWrote {} ({:.2} s)", out_path.display(), final_mix.frames() as f64 / sr);
    Ok(())
}
