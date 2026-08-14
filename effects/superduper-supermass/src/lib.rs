//! SuperDuper Supermass — standalone CLAP plugin wrapping the Supermassive-style
//! cascade reverb from `synth-core`. Long shimmering ambient tail intended for
//! pads, cinematic textures, vocals over Trance/ambient productions.
//!
//! The reverb network itself is fixed-geometry (size/decay are baked at
//! activation time — fundsp graphs can't be resized in the audio thread
//! without reallocating). Runtime-tweakable parameters are:
//!
//!   - Mix    (dry/wet blend)
//!   - Width  (stereo width of the wet)
//!   - Drive  (input saturation feeding the reverb)
//!   - Tilt   (high-shelf tilt on the wet, brightness control)
//!   - Duck Amount / Attack / Release (same ducker as superduper-reverb)
//!
//! That keeps the heavy fundsp graph immutable during process() while still
//! giving the user enough control to sound-design without rebuilding.

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod presets;

use atomic_float::AtomicF32;
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_common::stream::{InputStream, OutputStream};
use clack_extensions::state::{PluginState, PluginStateImpl};

use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use fundsp::prelude::Net;
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{DcBlocker, Ducker, SmoothedParam, Tilt};
use superduper_synth_core::supermass;

// ---------------------------------------------------------------------------
// File-based debug logging (Dock-launched DAW swallows stderr).
// ---------------------------------------------------------------------------

fn init_logging() { superduper_dsp_sdk::log::init("supermass"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Parameter table.
// ---------------------------------------------------------------------------

use superduper_dsp_sdk::clap_helpers::ParamDef;

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Mix",          min: 0.0, max: 1.0,  default: 0.3,  unit: ""   },
    ParamDef { id: 1, name: b"Width",        min: 0.0, max: 1.0,  default: 1.0,  unit: ""   },
    ParamDef { id: 2, name: b"Drive",        min: 0.0, max: 1.0,  default: 0.0,  unit: ""   },
    ParamDef { id: 3, name: b"Tilt",         min: -1.0, max: 1.0, default: 0.0,  unit: ""   },
    ParamDef { id: 4, name: b"Duck Amount",  min: 0.0, max: 24.0, default: 0.0,  unit: "dB" },
    ParamDef { id: 5, name: b"Duck Attack",  min: 1.0, max: 200.0, default: 5.0, unit: "ms" },
    ParamDef { id: 6, name: b"Duck Release", min: 10.0, max: 1000.0, default: 200.0, unit: "ms" },
];

pub const P_MIX: usize = 0;
pub const P_WIDTH: usize = 1;
pub const P_DRIVE: usize = 2;
pub const P_TILT: usize = 3;
pub const P_DUCK_AMOUNT: usize = 4;
pub const P_DUCK_ATTACK: usize = 5;
pub const P_DUCK_RELEASE: usize = 6;


// Ducker, Tilt, DcBlocker and SmoothedParam come from synth-core/dsp_blocks
// (imported above). Two-line types here used to be 60 lines duplicated
// between reverb and supermass — moved out once we had ≥2 effects sharing them.

// ---------------------------------------------------------------------------
// CLAP plugin
// ---------------------------------------------------------------------------

/// Atomics in Arc so the GUI thread holds its own clone (same pattern as
/// superduper-reverb).
pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: std::sync::atomic::AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub dirty_params: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_begin: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_end: [std::sync::atomic::AtomicBool; PARAMS.len()],
    /// Currently-selected preset index — persisted via simple_state
    /// so the dropdown survives project reopens.
    pub active_preset: std::sync::atomic::AtomicU32,
}

pub struct PluginShared {
    pub inner: SharedParams,
}

impl PluginShared {
    fn new() -> Self {
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
            }),
        }
    }
    pub fn shared_handle(&self) -> SharedParams { std::sync::Arc::clone(&self.inner) }
}

impl Default for PluginShared {
    fn default() -> Self { Self::new() }
}

impl std::ops::Deref for PluginShared {
    type Target = SharedParamsInner;
    fn deref(&self) -> &SharedParamsInner { &self.inner }
}

impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    net: Net,
    // DC blockers in front of the fundsp graph — same rationale as in the
    // Dattorro plate: a few sustained seconds of DC offset accumulates in the
    // FDN feedback loops and drowns the tail in a static hum.
    dc_l: DcBlocker,
    dc_r: DcBlocker,
    tilt_l: Tilt,
    tilt_r: Tilt,
    ducker: Ducker,
    smooth_mix: SmoothedParam,
    smooth_width: SmoothedParam,
    smooth_drive: SmoothedParam,
    smooth_tilt: SmoothedParam,
    smooth_duck: SmoothedParam,
    sc_l: Box<[f32]>,
    sc_r: Box<[f32]>,
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
        slog!(
            "activate: sr={}, frames={}..={}",
            audio_config.sample_rate, audio_config.min_frames_count, audio_config.max_frames_count
        );
        let mut net = supermass::build_wet();
        // Critical: tell the fundsp graph the actual sample rate. Without
        // this its reverb delay lines are sized for fundsp's default 44100.
        use fundsp::audiounit::AudioUnit;
        net.set_sample_rate(audio_config.sample_rate);

        let max_frames = audio_config.max_frames_count as usize;
        // Snap smoothers to host-loaded values so first block isn't a fade-in.
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        Ok(Self {
            shared,
            net,
            dc_l: DcBlocker::default(),
            dc_r: DcBlocker::default(),
            tilt_l: Tilt::default(),
            tilt_r: Tilt::default(),
            ducker: Ducker::default(),
            smooth_mix: SmoothedParam::new(load(P_MIX)),
            smooth_width: SmoothedParam::new(load(P_WIDTH)),
            smooth_drive: SmoothedParam::new(load(P_DRIVE)),
            smooth_tilt: SmoothedParam::new(load(P_TILT)),
            smooth_duck: SmoothedParam::new(load(P_DUCK_AMOUNT)),
            sc_l: vec![0.0; max_frames].into_boxed_slice(),
            sc_r: vec![0.0; max_frames].into_boxed_slice(),
            sample_rate: audio_config.sample_rate as f32,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        // Flush denormals to zero — long decays / feedback loops
        // otherwise generate ≈10⁻³⁸ floats that murder CPU and cause
        // periodic ticks at the buffer rate. RAII restores host CSR.
        let _denormals = superduper_dsp_sdk::denormals::Guard::new();
        apply_param_events(self.shared, events.input);
        superduper_dsp_sdk::clap_helpers::emit_dirty_param_events(
            &self.shared.params, &self.shared.dirty_params, events.output);
        superduper_dsp_sdk::clap_helpers::emit_gesture_events(
            &self.shared.gesture_begin,
            &self.shared.gesture_end,
            events.output,
        );

        let mix = self.shared.params[P_MIX].load(Ordering::Relaxed);
        let width = self.shared.params[P_WIDTH].load(Ordering::Relaxed);
        let drive = self.shared.params[P_DRIVE].load(Ordering::Relaxed);
        let tilt = self.shared.params[P_TILT].load(Ordering::Relaxed);
        let duck_amount = self.shared.params[P_DUCK_AMOUNT].load(Ordering::Relaxed);
        let duck_attack = self.shared.params[P_DUCK_ATTACK].load(Ordering::Relaxed);
        let duck_release = self.shared.params[P_DUCK_RELEASE].load(Ordering::Relaxed);
        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        let sr = self.sample_rate;

        let frames = audio.frames_count() as usize;
        let n_frames = frames.min(self.sc_l.len());

        // ---- Step 1: snapshot sidechain (port 1) ----
        let mut sc_present = false;
        if let Some(sc_port) = audio.input_port(1) {
            if let Some(chans) = sc_port.channels()?.into_f32() {
                if let Some(l) = chans.channel(0) {
                    let n = n_frames.min(l.len());
                    self.sc_l[..n].copy_from_slice(&l[..n]);
                    if l.iter().take(n).any(|&x| x != 0.0) { sc_present = true; }
                }
                if let Some(r) = chans.channel(1) {
                    let n = n_frames.min(r.len());
                    self.sc_r[..n].copy_from_slice(&r[..n]);
                    if r.iter().take(n).any(|&x| x != 0.0) { sc_present = true; }
                } else {
                    self.sc_r[..n_frames].copy_from_slice(&self.sc_l[..n_frames]);
                }
            }
        }

        // ---- Step 2: process main port ----
        if let Some(mut main_pair) = audio.port_pair(0) {
            let Some(channel_pairs) = main_pair.channels()?.into_f32() else {
                return Ok(ProcessStatus::Continue);
            };
            let mut iter = channel_pairs.into_iter();
            let Some(ch_l) = iter.next() else { return Ok(ProcessStatus::Continue); };
            let ch_r = iter.next();

            let sc = if sc_present {
                Some((&self.sc_l[..n_frames], &self.sc_r[..n_frames]))
            } else {
                None
            };

            stereo_process(
                &mut self.net,
                &mut self.dc_l,
                &mut self.dc_r,
                &mut self.tilt_l,
                &mut self.tilt_r,
                &mut self.ducker,
                &mut self.smooth_mix,
                &mut self.smooth_width,
                &mut self.smooth_drive,
                &mut self.smooth_tilt,
                &mut self.smooth_duck,
                ch_l, ch_r, sc,
                sr, mix, width, drive, tilt,
                duck_amount, duck_attack, duck_release,
                bypassed,
                &self.shared.scope,
            );
        }

        Ok(ProcessStatus::Continue)
    }
}

#[allow(clippy::too_many_arguments)]
fn stereo_process(
    net: &mut Net,
    dc_l: &mut DcBlocker,
    dc_r: &mut DcBlocker,
    tilt_l: &mut Tilt,
    tilt_r: &mut Tilt,
    ducker: &mut Ducker,
    smooth_mix: &mut SmoothedParam,
    smooth_width: &mut SmoothedParam,
    smooth_drive: &mut SmoothedParam,
    smooth_tilt: &mut SmoothedParam,
    smooth_duck: &mut SmoothedParam,
    ch_l: ChannelPair<'_, f32>,
    ch_r: Option<ChannelPair<'_, f32>>,
    sc: Option<(&[f32], &[f32])>,
    sr: f32,
    mix_target: f32, width_target: f32, drive_target: f32, tilt_target: f32,
    duck_amount_target: f32, duck_attack: f32, duck_release: f32,
    bypassed: bool,
    scope: &superduper_synth_core::gui::LiveScope,
) {
    use superduper_dsp_sdk::clap_helpers::split_io;
    let Some((l_read, l_write)) = split_io(ch_l) else { return };
    let r = ch_r.and_then(split_io);

    // Mono case.
    let Some((r_read, r_write)) = r else {
        if bypassed {
            l_write.copy_from_slice(l_read);
            return;
        }
        let mut in_buf = [0.0_f32; 2];
        let mut out_buf = [0.0_f32; 2];
        use fundsp::audiounit::AudioUnit;
        for (i, (inp, o)) in l_read.iter().zip(l_write.iter_mut()).enumerate() {
            let mix = smooth_mix.step(mix_target, sr);
            let width = smooth_width.step(width_target, sr);
            let drive = smooth_drive.step(drive_target, sr);
            let tilt = smooth_tilt.step(tilt_target, sr);
            let duck_amount = smooth_duck.step(duck_amount_target, sr);

            let dry = *inp;
            let key = sc.map(|(s, _)| s.get(i).copied().unwrap_or(0.0)).unwrap_or(dry);
            let duck_gain = ducker.process(key, key, sr, duck_amount, duck_attack, duck_release);
            // DC block first, then drive — order matters because tanh on a
            // DC offset adds harmonics around DC that ALSO accumulate in the FDN.
            let cleaned = dc_l.process(dry);
            let driven = drive_sample(cleaned, drive);
            in_buf[0] = driven; in_buf[1] = driven;
            net.tick(&in_buf, &mut out_buf);
            let wet = tilt_l.process((out_buf[0] + out_buf[1]) * 0.5, sr, tilt);
            let final_out = dry * (1.0 - mix) + wet * width * duck_gain * mix;
            *o = final_out;
            scope.push(final_out);
        }
        return;
    };

    if bypassed {
        l_write.copy_from_slice(l_read);
        r_write.copy_from_slice(r_read);
        return;
    }

    let n = l_read.len().min(r_read.len());
    let mut in_buf = [0.0_f32; 2];
    let mut out_buf = [0.0_f32; 2];
    use fundsp::audiounit::AudioUnit;
    for i in 0..n {
        let mix = smooth_mix.step(mix_target, sr);
        let width = smooth_width.step(width_target, sr);
        let drive = smooth_drive.step(drive_target, sr);
        let tilt = smooth_tilt.step(tilt_target, sr);
        let duck_amount = smooth_duck.step(duck_amount_target, sr);

        let dl = l_read[i];
        let dr = r_read[i];
        let (key_l, key_r) = match sc {
            Some((sl, sr_buf)) => (
                sl.get(i).copied().unwrap_or(0.0),
                sr_buf.get(i).copied().unwrap_or(0.0),
            ),
            None => (dl, dr),
        };
        let duck_gain = ducker.process(key_l, key_r, sr, duck_amount, duck_attack, duck_release);

        // DC block per channel, then drive.
        let cl = dc_l.process(dl);
        let cr = dc_r.process(dr);
        in_buf[0] = drive_sample(cl, drive);
        in_buf[1] = drive_sample(cr, drive);
        net.tick(&in_buf, &mut out_buf);

        let wet_l = tilt_l.process(out_buf[0], sr, tilt);
        let wet_r = tilt_r.process(out_buf[1], sr, tilt);

        let mono_w = (wet_l + wet_r) * 0.5;
        let final_wl = wet_l * width + mono_w * (1.0 - width);
        let final_wr = wet_r * width + mono_w * (1.0 - width);

        let out_l_sample = dl * (1.0 - mix) + final_wl * duck_gain * mix;
        l_write[i] = out_l_sample;
        let out_r_sample = dr * (1.0 - mix) + final_wr * duck_gain * mix;
        r_write[i] = out_r_sample;
        scope.push((out_l_sample + out_r_sample) * 0.5);
    }
}

#[inline]
fn drive_sample(x: f32, amount: f32) -> f32 {
    if amount <= 0.001 { return x; }
    let g = 1.0 + amount * 4.0;
    (x * g).tanh() / (1.0 + 0.5 * amount)
}

// ---------------------------------------------------------------------------
// Audio ports (main I/O + sidechain) and params extensions.
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 {
        if is_input { 2 } else { 1 }
    }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        match (index, is_input) {
            (0, _) => writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: if is_input { b"Input" } else { b"Output" },
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                // No in-place: the host must hand us separate input and output
                // buffers. With in-place allowed, split_io had to return a
                // &[f32] and a &mut [f32] over the SAME memory to keep the
                // "read x[i], write y[i]" style working — two noalias slices
                // aliasing each other, which is undefined behaviour whatever
                // the access order. The host's own copy costs the same as the
                // scratch buffer we would otherwise keep per plugin.
                in_place_pair: None,
            }),
            (1, true) => writer.set(&AudioPortInfo {
                id: ClapId::new(1),
                name: b"Sidechain",
                channel_count: 2,
                flags: AudioPortFlags::empty(),
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            }),
            _ => {}
        }
    }
}

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 { PARAMS.len() as u32 }

    fn get_info(&mut self, param_index: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info(PARAMS, param_index, info);
    }

    fn get_value(&mut self, param_id: ClapId) -> Option<f64> {
        let i = param_id.get() as usize;
        self.shared.params.get(i).map(|a| a.load(Ordering::Relaxed) as f64)
    }

    fn value_to_text(&mut self, param_id: ClapId, value: f64, writer: &mut ParamDisplayWriter) -> core::fmt::Result {
        ParamDef::write_display(PARAMS, param_id, value, writer)
    }

    fn text_to_value(&mut self, param_id: ClapId, text: &CStr) -> Option<f64> {
        ParamDef::parse_text(PARAMS, param_id, text)
    }

    fn flush(&mut self, input_events: &InputEvents, _output_events: &mut OutputEvents) {
        apply_param_events(self.shared, input_events);
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, input_events: &InputEvents, _output_events: &mut OutputEvents) {
        apply_param_events(self.shared, input_events);
    }
}

// ---------------------------------------------------------------------------
// CLAP state — params + bypass through the shared SDK helper. Without this
// REAPER drops everything when saving the project / FX chain preset.
// ---------------------------------------------------------------------------

superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);


// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct SuperDuperSupermass;

impl Plugin for SuperDuperSupermass {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<clack_extensions::gui::PluginGui>();
    }
}

impl DefaultPluginFactory for SuperDuperSupermass {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.supermass",
            plugin_display_name!("SuperDuper Supermass"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Valhalla Supermassive-style cascade reverb")
        .with_features([AUDIO_EFFECT, STEREO, REVERB])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Supermass — build {} ({})", build_num!(), build_date!());
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

// ===========================================================================
// CLAP GUI extension — embedded egui_baseview window.
// ===========================================================================

use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window as ClapGuiWindow,
};
use std::sync::atomic::Ordering as AtomicOrdering;

impl PluginGuiImpl for PluginMainThread<'_> {
    fn is_api_supported(&mut self, config: GuiConfiguration) -> bool {
        if config.is_floating { return false; }
        config.api_type == GuiApiType::COCOA
            || config.api_type == GuiApiType::WIN32
            || config.api_type == GuiApiType::X11
    }

    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        let api_type = if cfg!(target_os = "macos") {
            GuiApiType::COCOA
        } else if cfg!(target_os = "windows") {
            GuiApiType::WIN32
        } else {
            GuiApiType::X11
        };
        Some(GuiConfiguration { api_type, is_floating: false })
    }

    fn create(&mut self, _config: GuiConfiguration) -> Result<(), PluginError> {
        slog!("gui::create");
        Ok(())
    }

    fn destroy(&mut self) {
        slog!("gui::destroy");
        self.gui_handle = None;
    }

    fn set_scale(&mut self, _scale: f64) -> Result<(), PluginError> { Ok(()) }

    fn get_size(&mut self) -> Option<GuiSize> {
        Some(GuiSize {
            width: self.gui_resize.0.load(AtomicOrdering::Relaxed),
            height: self.gui_resize.1.load(AtomicOrdering::Relaxed),
        })
    }

    fn can_resize(&mut self) -> bool { true }

    fn get_resize_hints(&mut self) -> Option<GuiResizeHints> {
        Some(GuiResizeHints {
            can_resize_horizontally: true,
            can_resize_vertically: true,
            strategy: AspectRatioStrategy::Disregard,
        })
    }

    fn adjust_size(&mut self, size: GuiSize) -> Option<GuiSize> {
        Some(GuiSize {
            width: size.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH),
            height: size.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT),
        })
    }

    fn set_size(&mut self, size: GuiSize) -> Result<(), PluginError> {
        let w = size.width.clamp(gui::MIN_WIDTH, gui::MAX_WIDTH);
        let h = size.height.clamp(gui::MIN_HEIGHT, gui::MAX_HEIGHT);
        self.gui_resize.0.store(w, AtomicOrdering::Relaxed);
        self.gui_resize.1.store(h, AtomicOrdering::Relaxed);
        Ok(())
    }

    fn set_parent(&mut self, window: ClapGuiWindow) -> Result<(), PluginError> {
        slog!("gui::set_parent (api={:?})", window.api_type());
        let shared = self.shared.shared_handle();
        let handle = gui::open_window(&window, shared, self.gui_resize.clone());
        self.gui_handle = Some(handle);
        Ok(())
    }

    fn set_transient(&mut self, _window: ClapGuiWindow) -> Result<(), PluginError> { Ok(()) }
    fn show(&mut self) -> Result<(), PluginError> { slog!("gui::show"); Ok(()) }
    fn hide(&mut self) -> Result<(), PluginError> { slog!("gui::hide"); Ok(()) }
}

clack_export_entry!(SinglePluginEntry<SuperDuperSupermass>);
