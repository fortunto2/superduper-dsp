//! SuperDuper EQ — transparent 3-band parametric.
//!
//! Topology: HP → Low Shelf → Mid Peak → High Shelf → LP. Each band is a
//! single RBJ biquad (see `superduper_synth_core::dsp_blocks::Biquad`).
//! Coefficients are re-computed once per block when any band's params
//! change (cheap enough that we just always do it).
//!
//! Stereo: per-channel state, identical coefficients on L and R.

#![allow(clippy::missing_safety_doc)]

pub mod dynamic;
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
use clack_extensions::state::PluginState;

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

fn init_logging() { superduper_dsp_sdk::log::init("eq"); }
use superduper_dsp_sdk::slog;

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
    // Dynamic Mid (appended so old projects keep their values). Dyn Gain 0 = off.
    ParamDef { id: 10, name: b"Dyn Gain",  min: -18.0, max: 0.0,     default: 0.0,    unit: "dB" },
    ParamDef { id: 11, name: b"Dyn Range", min: 6.0,   max: 40.0,    default: 24.0,   unit: "dB" },
    // 0 = Key (the bus key track's level at Mid Freq), 1 = Self (the band's own level).
    ParamDef { id: 12, name: b"Dyn Source", min: 0.0,  max: 1.0,     default: 0.0,    unit: ""   },
];

/// Enum params the host must quantise instead of sweeping.
pub const STEPPED_PARAMS: &[u32] = &[12];

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
pub const P_DYN_GAIN: usize = 10;
pub const P_DYN_RANGE: usize = 11;
pub const P_DYN_SOURCE: usize = 12;

// ---------------------------------------------------------------------------
// Shared
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
    /// Currently-selected preset index — persisted via simple_state
    /// so the dropdown survives project reopens.
    pub active_preset: std::sync::atomic::AtomicU32,
    /// The dynamic band's current gain offset (dB, ≤ 0), for the GUI's ghost curve.
    pub dyn_now: AtomicF32,
}

pub struct PluginShared { pub inner: SharedParams }

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
                dyn_now: AtomicF32::new(0.0),
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(4096),
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
    track_watch: superduper_synth_core::gui::TrackInfoWatch<'a>,
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    dsp: EqDsp,
}

/// The EQ's DSP, separate from the CLAP wrapper so tests can drive it with
/// plain slices and a hand-set bus key.
pub struct EqDsp {
    chain_l: [Biquad; 5], // HP, LowShelf, MidPeak, HighShelf, LP
    chain_r: [Biquad; 5],
    smooth_output: SmoothedParam,
    sample_rate: f32,
    dynamic: dynamic::DynBand,
}

/// One channel's input and output for a block.
pub type Io<'b> = (&'b [f32], &'b mut [f32]);

/// Block-constant values read from the atomics once per block.
struct Block { hp_on: bool, lp_on: bool, out_db: f32, mid_f: f32, mid_g: f32, mid_q: f32 }

#[inline]
fn run_chain(chain: &mut [Biquad; 5], x: f32, b: &Block) -> f32 {
    let mut y = x;
    if b.hp_on { y = chain[0].process(y); }
    y = chain[1].process(y);
    y = chain[2].process(y);
    y = chain[3].process(y);
    if b.lp_on { y = chain[4].process(y); }
    y
}

impl EqDsp {
    pub fn new(sample_rate: f32, output_db: f32) -> Self {
        Self {
            chain_l: Default::default(),
            chain_r: Default::default(),
            smooth_output: SmoothedParam::new(output_db),
            sample_rate,
            dynamic: dynamic::DynBand::default(),
        }
    }

    /// One block, stereo (or mono when `r` is None). Reads params, the bus
    /// key and bypass from `shared`; writes the dynamic offset back for the GUI.
    pub fn process(&mut self, shared: &SharedParamsInner, l: Io<'_>, r: Option<Io<'_>>) {
        let bypassed = shared.bypass.load(Ordering::Relaxed);
        let sr = self.sample_rate;
        let p = |i: usize| shared.params[i].load(Ordering::Relaxed);

        // Coefficient update — recompute every block. Biquad coefs are cheap
        // (5 mul + 4 add) and we already had to read the atomics.
        let (lo_f, lo_g, hi_f, hi_g, hp, lp) =
            (p(P_LOW_FREQ), p(P_LOW_GAIN), p(P_HIGH_FREQ), p(P_HIGH_GAIN), p(P_HP), p(P_LP));
        let b = Block {
            hp_on: hp >= 20.0,
            lp_on: lp >= 20.0 && lp < sr * 0.45,
            out_db: p(P_OUTPUT),
            mid_f: p(P_MID_FREQ),
            mid_g: p(P_MID_GAIN),
            mid_q: p(P_MID_Q),
        };
        // HP at 0 Hz = off; we still feed identity coefs to keep state moving.
        for chain in [&mut self.chain_l, &mut self.chain_r] {
            if b.hp_on { chain[0].set_hpf(sr, hp, 0.707); }
            chain[1].set_low_shelf(sr, lo_f, 1.0, lo_g);
            chain[2].set_peaking(sr, b.mid_f, b.mid_q, b.mid_g);
            chain[3].set_high_shelf(sr, hi_f, 1.0, hi_g);
            if b.lp_on { chain[4].set_lpf(sr, lp, 0.707); }
        }

        let dyn_gain = p(P_DYN_GAIN);
        if bypassed || (dyn_gain >= 0.0 && self.dynamic.is_idle()) {
            // The static EQ, exactly as it was before the dynamic band.
            self.process_static(shared, l, &b, bypassed, 0);
            if let Some(r) = r { self.process_static(shared, r, &b, bypassed, 1); }
            if bypassed { self.dynamic.offset_db = 0.0; }
        } else {
            let level = if p(P_DYN_SOURCE) >= 0.5 {
                None // self: measured per chunk inside
            } else {
                Some(shared.scope.key_db_at(b.mid_f))
            };
            self.process_dynamic(shared, l, r, &b, level, dyn_gain.min(0.0), p(P_DYN_RANGE));
        }
        shared.dyn_now.store(self.dynamic.offset_db, Ordering::Relaxed);
    }

    fn process_static(&mut self, shared: &SharedParamsInner, (read, write): Io<'_>, b: &Block, bypassed: bool, ch: usize) {
        if bypassed {
            write.copy_from_slice(read);
            return;
        }
        let sr = self.sample_rate;
        let chain = if ch == 0 { &mut self.chain_l } else { &mut self.chain_r };
        for (i, o) in read.iter().zip(write.iter_mut()) {
            let out_db = self.smooth_output.step(b.out_db, sr);
            let out_lin = 10f32.powf(out_db / 20.0);
            let final_out = run_chain(chain, *i, b) * out_lin;
            *o = final_out;
            if ch == 0 { shared.scope.push(final_out); } // one channel: L then R blocks read as seams
        }
    }

    /// Both channels together, chunk by chunk, so L and R share the moving
    /// bell. `key_level` is the bus key's level at Mid Freq, or None to
    /// detect the band's own input level.
    #[allow(clippy::too_many_arguments)]
    fn process_dynamic(
        &mut self, shared: &SharedParamsInner, (l_in, l_out): Io<'_>, mut r: Option<Io<'_>>,
        b: &Block, key_level: Option<f32>, dyn_gain: f32, range: f32,
    ) {
        let sr = self.sample_rate;
        let det_coef = dynamic::DynBand::detect_coef(sr);
        if key_level.is_none() { self.dynamic.tune(sr, b.mid_f, b.mid_q); }
        let n = l_in.len().min(l_out.len());
        let mut start = 0;
        while start < n {
            let end = (start + dynamic::CHUNK).min(n);
            let level = match key_level {
                Some(k) => k,
                None => {
                    for (i, &xl) in l_in[start..end].iter().enumerate() {
                        let x = match &r { Some((ri, _)) => 0.5 * (xl + ri.get(start + i).copied().unwrap_or(0.0)), None => xl };
                        self.dynamic.detect(x, det_coef);
                    }
                    self.dynamic.self_level_db()
                }
            };
            let off = self.dynamic.step(dynamic::target_db(level, dyn_gain, range), end - start, sr);
            self.chain_l[2].set_peaking(sr, b.mid_f, b.mid_q, b.mid_g + off);
            self.chain_r[2].set_peaking(sr, b.mid_f, b.mid_q, b.mid_g + off);
            for i in start..end {
                let out_db = self.smooth_output.step(b.out_db, sr);
                let out_lin = 10f32.powf(out_db / 20.0);
                let yl = run_chain(&mut self.chain_l, l_in[i], b) * out_lin;
                l_out[i] = yl;
                shared.scope.push(yl);
                if let Some((ri, ro)) = r.as_mut() {
                    if let (Some(x), Some(o)) = (ri.get(i), ro.get_mut(i)) {
                        *o = run_chain(&mut self.chain_r, *x, b) * out_lin;
                    }
                }
            }
            start = end;
        }
    }
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
        shared.scope.set_sample_rate(audio_config.sample_rate as f32);
        let sr = audio_config.sample_rate as f32;
        slog!("activate sr={}", sr);
        Ok(Self {
            shared,
            dsp: EqDsp::new(sr, shared.params[P_OUTPUT].load(Ordering::Relaxed)),
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

        use superduper_dsp_sdk::clap_helpers::split_io;
        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            let mut chans = channel_pairs.into_iter();
            let Some(l) = chans.next().and_then(split_io) else { continue };
            let r = chans.next().and_then(split_io);
            self.dsp.process(self.shared, l, r);
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
            // No in-place — see the note in the sibling plugins: it existed
            // only to let split_io alias one buffer as both input and output.
            in_place_pair: None,
        });
    }
}

impl PluginMainThreadParams for PluginMainThread<'_> {
    fn count(&mut self) -> u32 { PARAMS.len() as u32 }
    fn get_info(&mut self, idx: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info_stepped(PARAMS, idx, info, STEPPED_PARAMS);
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

superduper_dsp_sdk::simple_state_impl!(PluginMainThread<'_>);


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
            .register::<clack_extensions::gui::PluginGui>()
            .register::<clack_extensions::track_info::PluginTrackInfo>();
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
        let shared = PluginShared::new();
        superduper_synth_core::gui::spawn_bus_publisher_mcp("SuperDuper EQ", &shared.inner, |s| &s.scope, Some(superduper_synth_core::gui::ParamsAccess { defs: PARAMS, values: |s| &s.params, dirty: Some(|s| &s.dirty_params) }), None);
        Ok(shared)
    }

    fn new_main_thread<'a>(
        host: HostMainThreadHandle<'a>,
        shared: &'a PluginShared,
    ) -> Result<PluginMainThread<'a>, PluginError> {
        let track_watch = superduper_synth_core::gui::TrackInfoWatch::new(host, &shared.scope);
        Ok(PluginMainThread {
            track_watch,
            shared,
            gui_handle: None,
            gui_resize: gui::new_resize_bridge(),
        })
    }
}

clack_export_entry!(SinglePluginEntry<SuperDuperEq>);

impl clack_extensions::track_info::PluginTrackInfoImpl for PluginMainThread<'_> {
    fn changed(&self) {
        self.track_watch.refresh();
    }
}
