//! SuperDuper EQ — transparent 3-band parametric.
//!
//! Topology: HP → Low Shelf → Mid Peak → High Shelf → LP. Each band is a
//! single RBJ biquad (see `superduper_synth_core::dsp_blocks::Biquad`).
//! Coefficients are re-computed once per block when any band's params
//! change (cheap enough that we just always do it).
//!
//! Stereo: per-channel state, identical coefficients on L and R.

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
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams,
    PluginParams,
};
use clack_common::stream::{InputStream, OutputStream};
use clack_extensions::state::{PluginState, PluginStateImpl};

use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use std::ffi::CStr;
use std::sync::atomic::Ordering;
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{Biquad, SmoothedParam};

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

fn log_path() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".superduper-dsp")
        .join("eq.log")
}

static LOG_FILE: std::sync::OnceLock<parking_lot::Mutex<Option<std::fs::File>>> =
    std::sync::OnceLock::new();

fn init_logging() {
    LOG_FILE.get_or_init(|| {
        let path = log_path();
        let _ = std::fs::create_dir_all(path.parent().unwrap());
        let file = std::fs::OpenOptions::new().create(true).append(true).open(&path).ok();
        parking_lot::Mutex::new(file)
    });
}
fn slog_args(args: std::fmt::Arguments<'_>) {
    use std::io::Write;
    if let Some(slot) = LOG_FILE.get() {
        if let Some(file) = slot.lock().as_mut() {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis()).unwrap_or(0);
            let _ = writeln!(file, "[{}] {}", now, args);
        }
    }
}
macro_rules! slog { ($($arg:tt)*) => { $crate::slog_args(format_args!($($arg)*)) } }

// ---------------------------------------------------------------------------
// Params — 3 bands + HP/LP + output
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    // Low shelf
    ParamDef { id: 0, name: b"Low Freq",  min: 30.0,   max: 500.0,   default: 120.0,  unit: "Hz" },
    ParamDef { id: 1, name: b"Low Gain",  min: -15.0,  max: 15.0,    default: 0.0,    unit: "dB" },
    // Mid peak
    ParamDef { id: 2, name: b"Mid Freq",  min: 200.0,  max: 5000.0,  default: 1000.0, unit: "Hz" },
    ParamDef { id: 3, name: b"Mid Gain",  min: -15.0,  max: 15.0,    default: 0.0,    unit: "dB" },
    ParamDef { id: 4, name: b"Mid Q",     min: 0.3,    max: 6.0,     default: 0.7,    unit: ""   },
    // High shelf
    ParamDef { id: 5, name: b"High Freq", min: 2000.0, max: 18000.0, default: 8000.0, unit: "Hz" },
    ParamDef { id: 6, name: b"High Gain", min: -15.0,  max: 15.0,    default: 0.0,    unit: "dB" },
    // HP / LP (toggleable via Hz min/max — 0 means "off")
    ParamDef { id: 7, name: b"HP",        min: 0.0,    max: 500.0,   default: 0.0,    unit: "Hz" },
    ParamDef { id: 8, name: b"LP",        min: 0.0,    max: 22000.0, default: 0.0,    unit: "Hz" },
    // Output trim
    ParamDef { id: 9, name: b"Output",    min: -12.0,  max: 12.0,    default: 0.0,    unit: "dB" },
];

pub const P_LOW_FREQ: usize = 0;
pub const P_LOW_GAIN: usize = 1;
pub const P_MID_FREQ: usize = 2;
pub const P_MID_GAIN: usize = 3;
pub const P_MID_Q: usize = 4;
pub const P_HIGH_FREQ: usize = 5;
pub const P_HIGH_GAIN: usize = 6;
pub const P_HP: usize = 7;
pub const P_LP: usize = 8;
pub const P_OUTPUT: usize = 9;

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: std::sync::atomic::AtomicBool,
    pub dirty_params: [std::sync::atomic::AtomicBool; PARAMS.len()],
}

pub struct PluginShared { pub inner: SharedParams }

impl PluginShared {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: std::sync::atomic::AtomicBool::new(false),
                dirty_params: std::array::from_fn(|_| std::sync::atomic::AtomicBool::new(false)),
            }),
        }
    }
    pub fn shared_handle(&self) -> SharedParams { std::sync::Arc::clone(&self.inner) }
}

impl Default for PluginShared { fn default() -> Self { Self::new() } }
impl std::ops::Deref for PluginShared {
    type Target = SharedParamsInner;
    fn deref(&self) -> &SharedParamsInner { &self.inner }
}
impl<'a> clack_plugin::plugin::PluginShared<'a> for PluginShared {}

// ---------------------------------------------------------------------------
// Audio processor — per-channel chain of five biquads.
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    chain_l: [Biquad; 5], // HP, LowShelf, MidPeak, HighShelf, LP
    chain_r: [Biquad; 5],
    smooth_output: SmoothedParam,
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
        let sr = audio_config.sample_rate as f32;
        slog!("activate sr={}", sr);
        Ok(Self {
            shared,
            chain_l: Default::default(),
            chain_r: Default::default(),
            smooth_output: SmoothedParam::new(
                shared.params[P_OUTPUT].load(Ordering::Relaxed),
            ),
            sample_rate: sr,
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
            &self.shared.params, &self.shared.dirty_params, events.output);

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        let sr = self.sample_rate;

        // Coefficient update — recompute every block. Biquad coefs are cheap
        // (5 mul + 4 add) and we already had to read the atomics.
        let lo_f = self.shared.params[P_LOW_FREQ].load(Ordering::Relaxed);
        let lo_g = self.shared.params[P_LOW_GAIN].load(Ordering::Relaxed);
        let mid_f = self.shared.params[P_MID_FREQ].load(Ordering::Relaxed);
        let mid_g = self.shared.params[P_MID_GAIN].load(Ordering::Relaxed);
        let mid_q = self.shared.params[P_MID_Q].load(Ordering::Relaxed);
        let hi_f = self.shared.params[P_HIGH_FREQ].load(Ordering::Relaxed);
        let hi_g = self.shared.params[P_HIGH_GAIN].load(Ordering::Relaxed);
        let hp = self.shared.params[P_HP].load(Ordering::Relaxed);
        let lp = self.shared.params[P_LP].load(Ordering::Relaxed);
        let output_db_target = self.shared.params[P_OUTPUT].load(Ordering::Relaxed);

        // HP at 0 Hz = off; we still feed identity coefs to keep state moving.
        if hp >= 20.0 {
            self.chain_l[0].set_hpf(sr, hp, 0.707);
            self.chain_r[0].set_hpf(sr, hp, 0.707);
        }
        self.chain_l[1].set_low_shelf(sr, lo_f, 1.0, lo_g);
        self.chain_r[1].set_low_shelf(sr, lo_f, 1.0, lo_g);
        self.chain_l[2].set_peaking(sr, mid_f, mid_q, mid_g);
        self.chain_r[2].set_peaking(sr, mid_f, mid_q, mid_g);
        self.chain_l[3].set_high_shelf(sr, hi_f, 1.0, hi_g);
        self.chain_r[3].set_high_shelf(sr, hi_f, 1.0, hi_g);
        if lp >= 20.0 && lp < sr * 0.45 {
            self.chain_l[4].set_lpf(sr, lp, 0.707);
            self.chain_r[4].set_lpf(sr, lp, 0.707);
        }

        let hp_on = hp >= 20.0;
        let lp_on = lp >= 20.0 && lp < sr * 0.45;

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            for (ch_idx, channel_pair) in channel_pairs.into_iter().enumerate() {
                let chain = if ch_idx == 0 { &mut self.chain_l } else { &mut self.chain_r };
                use superduper_dsp_sdk::clap_helpers::split_io;
                let Some((read, write)) = split_io(channel_pair) else { continue };
                if bypassed {
                    write.copy_from_slice(read);
                    continue;
                }
                for (i, o) in read.iter().zip(write.iter_mut()) {
                    let out_db = self.smooth_output.step(output_db_target, sr);
                    let out_lin = 10f32.powf(out_db / 20.0);
                    let mut y = *i;
                    if hp_on { y = chain[0].process(y); }
                    y = chain[1].process(y);
                    y = chain[2].process(y);
                    y = chain[3].process(y);
                    if lp_on { y = chain[4].process(y); }
                    *o = y * out_lin;
                }
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, _is_input: bool) -> u32 { 1 }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index != 0 { return; }
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
    fn count(&mut self) -> u32 { PARAMS.len() as u32 }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info(PARAMS, idx, info);
    }
    fn get_value(&mut self, id: ClapId) -> Option<f64> {
        let i = id.get() as usize;
        self.shared.params.get(i).map(|a| a.load(Ordering::Relaxed) as f64)
    }
    fn value_to_text(&mut self, id: ClapId, v: f64, w: &mut ParamDisplayWriter) -> core::fmt::Result {
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

// ---------------------------------------------------------------------------
// CLAP state — params + bypass through the shared SDK helper. Without this
// REAPER drops everything when saving the project / FX chain preset.
// ---------------------------------------------------------------------------

impl PluginStateImpl for PluginMainThread<'_> {
    fn save(&mut self, output: &mut OutputStream) -> Result<(), PluginError> {
        superduper_dsp_sdk::clap_helpers::save_simple_state(
            &self.shared.params,
            self.shared.bypass.load(std::sync::atomic::Ordering::Relaxed),
            output,
        )
    }
    fn load(&mut self, input: &mut InputStream) -> Result<(), PluginError> {
        let bypass = superduper_dsp_sdk::clap_helpers::load_simple_state(
            &self.shared.params,
            input,
        )?;
        self.shared.bypass.store(bypass, std::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}


use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window as ClapGuiWindow,
};
use std::sync::atomic::Ordering as AtomicOrdering;

impl PluginGuiImpl for PluginMainThread<'_> {
    fn is_api_supported(&mut self, c: GuiConfiguration) -> bool {
        if c.is_floating { return false; }
        c.api_type == GuiApiType::COCOA
            || c.api_type == GuiApiType::WIN32
            || c.api_type == GuiApiType::X11
    }
    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        let api_type = if cfg!(target_os = "macos") { GuiApiType::COCOA }
            else if cfg!(target_os = "windows") { GuiApiType::WIN32 }
            else { GuiApiType::X11 };
        Some(GuiConfiguration { api_type, is_floating: false })
    }
    fn create(&mut self, _: GuiConfiguration) -> Result<(), PluginError> { slog!("gui::create"); Ok(()) }
    fn destroy(&mut self) { slog!("gui::destroy"); self.gui_handle = None; }
    fn set_scale(&mut self, _: f64) -> Result<(), PluginError> { Ok(()) }
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
        slog!("gui::set_parent");
        let handle = gui::open_window(&window, self.shared.shared_handle(), self.gui_resize.clone());
        self.gui_handle = Some(handle);
        Ok(())
    }
    fn set_transient(&mut self, _: ClapGuiWindow) -> Result<(), PluginError> { Ok(()) }
    fn show(&mut self) -> Result<(), PluginError> { Ok(()) }
    fn hide(&mut self) -> Result<(), PluginError> { Ok(()) }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct SuperDuperEq;

impl Plugin for SuperDuperEq {
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

impl DefaultPluginFactory for SuperDuperEq {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.eq",
            plugin_display_name!("SuperDuper EQ"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.2"))
        .with_description("3-band parametric EQ + HP/LP")
        .with_features([AUDIO_EFFECT, STEREO, EQUALIZER])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: EQ — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperEq>);
