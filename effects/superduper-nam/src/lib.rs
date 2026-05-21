//! SuperDuper NAM — Neural Amp Modeler plugin.
//!
//! Runs a small WaveNet (built into [`superduper_synth_core::nam`]) per
//! sample on each channel. Ships with a built-in hand-tuned "Warm Tube"
//! model so the plugin sounds like something out of the box — community
//! `.nam` loading is wired up but the canonical weight-fanout from the
//! NAM 0.5.x format is still in progress (parsing succeeds, weights are
//! validated, but full population of all layer tensors is a future
//! revision).
//!
//! Signal chain (per channel, per sample):
//!   in → Input gain → WaveNet (4 layers, 8 channels, dilations 1/2/4/8)
//!      → Output gain → Mix(dry/wet)

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod presets;

use atomic_float::AtomicF32;
use clack_common::stream::{InputStream, OutputStream};
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams,
    PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};

use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{DcBlocker, SmoothedParam};
use superduper_synth_core::nam::{Activation, LayerArrayParams, NamModel, WaveNet};

fn init_logging() {
    superduper_dsp_sdk::log::init("nam");
}
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Built-in model dimensions. Mirrors NAM "Lite" topology (channels=8,
// 4 dilations × 2 stacks) — ~1100 weights, well under 1% CPU at 48 kHz.
// ---------------------------------------------------------------------------

fn make_default_net() -> NamModel {
    let stacks = vec![
        LayerArrayParams {
            input_size: 1,
            condition_size: 1,
            channels: 8,
            bottleneck: 8,
            head_size: 4,
            head_kernel_size: 1,
            head_bias: false,
            kernel: 3,
            dilations: vec![1, 2, 4, 8],
            gating_mode: superduper_synth_core::nam::GatingMode::None,
            activation: Activation::Tanh,
            secondary_activation: Activation::Sigmoid,
            layer1x1_active: true,
            head1x1_out_channels: None,
        },
        LayerArrayParams {
            input_size: 8,
            condition_size: 1,
            channels: 8,
            bottleneck: 8,
            head_size: 1,
            head_kernel_size: 1,
            head_bias: true,
            kernel: 3,
            dilations: vec![1, 2, 4, 8],
            gating_mode: superduper_synth_core::nam::GatingMode::None,
            activation: Activation::Tanh,
            secondary_activation: Activation::Sigmoid,
            layer1x1_active: true,
            head1x1_out_channels: None,
        },
    ];
    let mut n = WaveNet::from_params(1, &stacks, 0.5);
    n.hand_tune_tube_preamp();
    NamModel::WaveNet(n)
}

/// Try to load a `.nam` file off the main thread. Returns the parsed
/// model (WaveNet or LSTM) plus a user-friendly name (the file stem).
pub fn try_load_nam(
    path: &std::path::Path,
) -> Result<(NamModel, String), Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let file = superduper_synth_core::nam::load_from_json(&text)?;
    let model = NamModel::from_nam_file(&file)?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("model")
        .to_string();
    Ok((model, name))
}

// ---------------------------------------------------------------------------
// Params (FROZEN once shipped — order/IDs cannot change)
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Input",  min: -24.0, max: 24.0, default: 0.0,  unit: "dB" },
    // Scales the WaveNet input projection weights, pushing the network
    // further into its non-linear region without changing the model
    // weights themselves. 1 = nominal, >1 = harder drive, <1 = clean.
    ParamDef { id: 1, name: b"Drive",  min: 0.0,   max: 12.0, default: 3.0,  unit: "dB" },
    ParamDef { id: 2, name: b"Output", min: -24.0, max: 24.0, default: 0.0,  unit: "dB" },
    ParamDef { id: 3, name: b"Mix",    min: 0.0,   max: 1.0,  default: 1.0,  unit: ""   },
    // Tone tilt applied after the network — same one-pole shelf as the
    // saturator's Tilt block. Positive = brighter, negative = darker.
    ParamDef { id: 4, name: b"Tone",   min: -1.0,  max: 1.0,  default: 0.0,  unit: ""   },
];

pub const P_INPUT: usize = 0;
pub const P_DRIVE: usize = 1;
pub const P_OUTPUT: usize = 2;
pub const P_MIX: usize = 3;
pub const P_TONE: usize = 4;

// ---------------------------------------------------------------------------
// Shared (Arc to GUI thread)
// ---------------------------------------------------------------------------

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: std::sync::atomic::AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub dirty_params: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_begin: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_end: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub active_preset: std::sync::atomic::AtomicU32,
    /// Last NAM file the user picked. GUI displays it; the audio
    /// thread doesn't read it (loading happens off-thread on the main
    /// thread and swaps the WaveNet via the message channel below).
    pub current_model_name: parking_lot::Mutex<String>,
    /// Drop-box for a freshly-loaded model. Main thread parses a
    /// `.nam` file and `lock + store` here. Audio thread `try_lock`s
    /// once per `process()`; if `Some`, swap into the live net.
    pub pending_net: parking_lot::Mutex<Option<NamModel>>,
}

pub struct PluginShared {
    pub inner: SharedParams,
}

impl PluginShared {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: std::sync::atomic::AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_begin: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                gesture_end: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
                active_preset: std::sync::atomic::AtomicU32::new(0),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
                current_model_name: parking_lot::Mutex::new("Warm Tube (built-in)".into()),
                pending_net: parking_lot::Mutex::new(None),
            }),
        }
    }
    pub fn shared_handle(&self) -> SharedParams {
        std::sync::Arc::clone(&self.inner)
    }
}

impl Default for PluginShared {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Deref for PluginShared {
    type Target = SharedParamsInner;
    fn deref(&self) -> &SharedParamsInner {
        &self.inner
    }
}

impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

// ---------------------------------------------------------------------------
// Plugin audio processor
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    /// Two independent network instances — one per channel. Each holds
    /// its own ring-buffer state so left/right run truly in parallel
    /// without phasing artefacts from shared internal history.
    net_l: NamModel,
    net_r: NamModel,
    dc_l: DcBlocker,
    dc_r: DcBlocker,
    /// Tilt EQ post-network for tone shaping.
    tilt_l: superduper_synth_core::dsp_blocks::Tilt,
    tilt_r: superduper_synth_core::dsp_blocks::Tilt,
    smooth_input: SmoothedParam,
    smooth_drive: SmoothedParam,
    smooth_output: SmoothedParam,
    smooth_mix: SmoothedParam,
    smooth_tone: SmoothedParam,
    sample_rate: f32,
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
}

impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, PluginShared, PluginMainThread<'a>>
    for PluginAudioProcessor<'a>
{
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut PluginMainThread<'a>,
        shared: &'a PluginShared,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        slog!("activate: sr={}", audio_config.sample_rate);
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        // If the user has a community .nam in their library, default to
        // it; otherwise fall back to the built-in tube preamp.
        let mut net = make_default_net();
        let lib = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(".superduper-dsp/nam/wavenet_a1_standard.nam");
        if lib.exists() {
            if let Ok((n, name)) = try_load_nam(&lib) {
                slog!("activate: loaded {} ({} params)", name, n.param_count());
                *shared.current_model_name.lock() = name;
                net = n;
            } else {
                slog!("activate: failed to load {:?}", lib);
            }
        }
        Ok(Self {
            shared,
            net_l: net.clone(),
            net_r: net,
            dc_l: DcBlocker::default(),
            dc_r: DcBlocker::default(),
            tilt_l: superduper_synth_core::dsp_blocks::Tilt::default(),
            tilt_r: superduper_synth_core::dsp_blocks::Tilt::default(),
            smooth_input: SmoothedParam::new(load(P_INPUT)),
            smooth_drive: SmoothedParam::new(load(P_DRIVE)),
            smooth_output: SmoothedParam::new(load(P_OUTPUT)),
            smooth_mix: SmoothedParam::new(load(P_MIX)),
            smooth_tone: SmoothedParam::new(load(P_TONE)),
            sample_rate: audio_config.sample_rate as f32,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        apply_param_events(self.shared, events.input);
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(
            &self.shared.params,
            &self.shared.dirty_params,
            events.output,
        );
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(
            &self.shared.gesture_begin,
            &self.shared.gesture_end,
            events.output,
        );

        // Swap in a freshly loaded model if the main thread dropped one
        // into the pending box. `try_lock` keeps us off the slow path
        // if no swap is pending — and if the GUI is mid-load the audio
        // thread silently keeps using the current net.
        if let Some(mut guard) = self.shared.pending_net.try_lock() {
            if let Some(new_net) = guard.take() {
                self.net_l = new_net.clone();
                self.net_r = new_net;
                slog!("process: model swapped in");
            }
        }

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        let input_target = self.shared.params[P_INPUT].load(Ordering::Relaxed);
        let drive_target = self.shared.params[P_DRIVE].load(Ordering::Relaxed);
        let output_target = self.shared.params[P_OUTPUT].load(Ordering::Relaxed);
        let mix_target = self.shared.params[P_MIX].load(Ordering::Relaxed);
        let tone_target = self.shared.params[P_TONE].load(Ordering::Relaxed);
        let sr = self.sample_rate;

        // Run the network ONCE per sample on a mono sum (L+R)/2 and
        // copy the wet result back to both channels. NAM models are
        // trained on mono guitar / vocal feeds — sdatkinson's own
        // plugin does the same. Halves CPU (was 2× independent net
        // instances per sample) and matches expected behaviour:
        // dropping a stereo bus into a guitar amp doesn't give you a
        // wider stereo amp, it gives you a mono amp on summed signal.
        // Dry path stays stereo so Mix < 1 preserves the original
        // image untouched.
        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else {
                continue;
            };
            let chans: Vec<_> = channel_pairs.into_iter().collect();
            process_stereo_mono_net(
                &mut self.net_l, // net_r is kept in sync via swap; left is the active one
                &mut self.dc_l,
                &mut self.dc_r,
                &mut self.tilt_l,
                &mut self.tilt_r,
                &mut self.smooth_input,
                &mut self.smooth_drive,
                &mut self.smooth_output,
                &mut self.smooth_mix,
                &mut self.smooth_tone,
                chans,
                sr,
                input_target,
                drive_target,
                output_target,
                mix_target,
                tone_target,
                bypassed,
                &self.shared.scope,
            );
        }
        Ok(ProcessStatus::Continue)
    }
}

#[allow(clippy::too_many_arguments)]
fn process_stereo_mono_net(
    net: &mut NamModel,
    dc_l: &mut DcBlocker,
    dc_r: &mut DcBlocker,
    tilt_l: &mut superduper_synth_core::dsp_blocks::Tilt,
    tilt_r: &mut superduper_synth_core::dsp_blocks::Tilt,
    smooth_input: &mut SmoothedParam,
    smooth_drive: &mut SmoothedParam,
    smooth_output: &mut SmoothedParam,
    smooth_mix: &mut SmoothedParam,
    smooth_tone: &mut SmoothedParam,
    chans: Vec<ChannelPair<'_, f32>>,
    sr: f32,
    input_target: f32,
    drive_target: f32,
    output_target: f32,
    mix_target: f32,
    tone_target: f32,
    bypassed: bool,
    scope: &superduper_synth_core::gui::LiveScope,
) {
    use superduper_dsp_sdk::clap_helpers::split_io;
    // Two channels expected (stereo plugin); split each.
    let mut iter = chans.into_iter();
    let Some(ch0) = iter.next() else { return };
    let ch1 = iter.next();
    let Some((read_l, write_l)) = split_io(ch0) else { return };
    let (read_r, write_r): (&[f32], Option<&mut [f32]>) = match ch1 {
        Some(c) => match split_io(c) {
            Some((r, w)) => (r, Some(w)),
            None => return,
        },
        None => (read_l, None),
    };
    if bypassed {
        write_l.copy_from_slice(read_l);
        if let Some(w) = write_r {
            w.copy_from_slice(read_r);
        }
        return;
    }

    let n = read_l.len();
    let mut maybe_write_r = write_r;
    for i in 0..n {
        let dry_l = read_l[i];
        let dry_r = if read_r.len() == n { read_r[i] } else { dry_l };

        let in_db = smooth_input.step(input_target, sr);
        let drv_db = smooth_drive.step(drive_target, sr);
        let out_db = smooth_output.step(output_target, sr);
        let mix = smooth_mix.step(mix_target, sr);
        let tone = smooth_tone.step(tone_target, sr);
        let in_lin = 10f32.powf(in_db / 20.0);
        let drv_lin = 10f32.powf(drv_db / 20.0);
        let out_lin = 10f32.powf(out_db / 20.0);

        // Mono inference path — sum L+R, DC-block, drive into network.
        let dry_mid = (dry_l + dry_r) * 0.5;
        let cleaned = dc_l.process(dry_mid);
        let _ = dc_r.process(dry_mid); // keep state in sync (zero-input idle)
        let x_in = cleaned * in_lin * drv_lin;
        let y_net = net.process(x_in);

        // Tone tilt + output gain applied per channel so a future stereo
        // tone control still works; today they're identical because both
        // tilts see the same input.
        let wet_l = tilt_l.process(y_net, sr, tone) * out_lin;
        let wet_r = tilt_r.process(y_net, sr, tone) * out_lin;

        let mixed_l = dry_l * (1.0 - mix) + wet_l * mix;
        let mixed_r = dry_r * (1.0 - mix) + wet_r * mix;

        write_l[i] = mixed_l;
        if let Some(ref mut w) = maybe_write_r {
            w[i] = mixed_r;
        }
        scope.push((mixed_l + mixed_r) * 0.5);
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 {
        1
    }
    fn get(&mut self, _index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: if is_input { b"Input" } else { b"Output" },
            channel_count: 2,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            in_place_pair: Some(ClapId::new(0)),
        });
    }
}

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 {
        PARAMS.len() as u32
    }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info(PARAMS, idx, info);
    }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        let i = id.get() as usize;
        self.shared
            .params
            .get(i)
            .map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(
        &mut self,
        id: ClapId,
        v: f64,
        w: &mut ParamDisplayWriter,
    ) -> core::fmt::Result {
        ParamDef::write_display(PARAMS, id, v, w)
    }
    fn text_to_value(&mut self, id: ClapId, t: &CStr) -> Option<f64> {
        ParamDef::parse_text(PARAMS, id, t)
    }
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        apply_param_events(self.shared, ev);
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        apply_param_events(self.shared, ev);
    }
}

superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);

// ---------------------------------------------------------------------------
// CLAP GUI extension
// ---------------------------------------------------------------------------

use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window as ClapGuiWindow,
};
use std::sync::atomic::Ordering as AtomicOrdering;

impl PluginGuiImpl for PluginMainThread<'_> {
    fn is_api_supported(&mut self, c: GuiConfiguration) -> bool {
        if c.is_floating {
            return false;
        }
        c.api_type == GuiApiType::COCOA
            || c.api_type == GuiApiType::WIN32
            || c.api_type == GuiApiType::X11
    }
    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        let api_type = if cfg!(target_os = "macos") {
            GuiApiType::COCOA
        } else if cfg!(target_os = "windows") {
            GuiApiType::WIN32
        } else {
            GuiApiType::X11
        };
        Some(GuiConfiguration {
            api_type,
            is_floating: false,
        })
    }
    fn create(&mut self, _: GuiConfiguration) -> Result<(), PluginError> {
        Ok(())
    }
    fn destroy(&mut self) {
        self.gui_handle = None;
    }
    fn set_scale(&mut self, _: f64) -> Result<(), PluginError> {
        Ok(())
    }
    fn get_size(&mut self) -> Option<GuiSize> {
        Some(GuiSize {
            width: self.gui_resize.0.load(AtomicOrdering::Relaxed),
            height: self.gui_resize.1.load(AtomicOrdering::Relaxed),
        })
    }
    fn can_resize(&mut self) -> bool {
        true
    }
    fn get_resize_hints(&mut self) -> Option<GuiResizeHints> {
        Some(GuiResizeHints {
            can_resize_horizontally: true,
            can_resize_vertically: true,
            strategy: AspectRatioStrategy::Disregard,
        })
    }
    fn adjust_size(&mut self, s: GuiSize) -> Option<GuiSize> {
        Some(GuiSize {
            width: s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH),
            height: s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT),
        })
    }
    fn set_size(&mut self, s: GuiSize) -> Result<(), PluginError> {
        let w = s.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH);
        let h = s.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT);
        self.gui_resize.0.store(w, AtomicOrdering::Relaxed);
        self.gui_resize.1.store(h, AtomicOrdering::Relaxed);
        Ok(())
    }
    fn set_parent(&mut self, window: ClapGuiWindow) -> Result<(), PluginError> {
        let handle = gui::open_window(&window, self.shared.shared_handle(), self.gui_resize.clone());
        self.gui_handle = Some(handle);
        Ok(())
    }
    fn set_transient(&mut self, _: ClapGuiWindow) -> Result<(), PluginError> {
        Ok(())
    }
    fn show(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
    fn hide(&mut self) -> Result<(), PluginError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct SuperDuperNam;

impl Plugin for SuperDuperNam {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>();
    }
}

impl DefaultPluginFactory for SuperDuperNam {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.nam",
            plugin_display_name!("SuperDuper NAM"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Neural Amp Modeler — WaveNet inference tube/preamp emulation.")
        .with_features([AUDIO_EFFECT, STEREO, DISTORTION])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!(
            "new_shared: NAM — build {} ({})",
            build_num!(),
            build_date!()
        );
        Ok(PluginShared::new())
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        shared: &'a PluginShared,
    ) -> Result<PluginMainThread<'a>, PluginError> {
        Ok(PluginMainThread {
            shared,
            gui_handle: None,
            gui_resize: gui::new_resize_bridge(),
        })
    }
}

clack_export_entry!(SinglePluginEntry<SuperDuperNam>);
