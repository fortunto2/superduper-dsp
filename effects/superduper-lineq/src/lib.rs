//! SuperDuper LinPhase EQ — 3-band linear-phase mastering EQ.
//!
//! Same band layout as SuperDuper EQ (low-shelf + mid-bell +
//! high-shelf + HP/LP cuts) but the response is a FIR convolved
//! version of the biquad target. Result: phase is constant across
//! the spectrum (no group-delay smearing through the bands).
//!
//! Cost: latency of `FIR_LEN/2` samples (≈ 21 ms at 48 kHz with
//! FIR_LEN=2048). Reported to the host via the CLAP `latency`
//! extension so PDC keeps tracks aligned.
//!
//! Best used as a SECOND-stage EQ on the master bus where you want
//! phase-coherent tonal balance (e.g. wide low-shelf for kick body
//! without smearing the transient relative to the snare on top).
//! Don't use as a tracking EQ — the 21 ms feedback through headphones
//! is uncomfortable.

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod presets;

use atomic_float::AtomicF32;
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::latency::{PluginLatency, PluginLatencyImpl};
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
use superduper_synth_core::dsp_blocks::Biquad;
use superduper_synth_core::linphase::{design_linear_phase_fir, DirectFirConvolver};

fn init_logging() { superduper_dsp_sdk::log::init("lineq"); }
use superduper_dsp_sdk::slog;

/// FIR length — 1024 taps gives ~11 ms latency at 48 kHz, fine
/// resolution down to ~50 Hz. 2048 = ~21 ms, 25 Hz. Mastering pref
/// = quality > latency.
pub const FIR_LEN: usize = 2048;

// ---------------------------------------------------------------------------
// Param table (same shape as SuperDuper EQ for muscle memory).
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"Low Freq",  min: 30.0,   max: 500.0,   default: 120.0,  unit: "Hz" },
    ParamDef { id: 1, name: b"Low Gain",  min: -15.0,  max: 15.0,    default: 0.0,    unit: "dB" },
    ParamDef { id: 2, name: b"Mid Freq",  min: 200.0,  max: 5000.0,  default: 1000.0, unit: "Hz" },
    ParamDef { id: 3, name: b"Mid Gain",  min: -15.0,  max: 15.0,    default: 0.0,    unit: "dB" },
    ParamDef { id: 4, name: b"Mid Q",     min: 0.3,    max: 6.0,     default: 0.7,    unit: ""   },
    ParamDef { id: 5, name: b"High Freq", min: 2000.0, max: 18000.0, default: 8000.0, unit: "Hz" },
    ParamDef { id: 6, name: b"High Gain", min: -15.0,  max: 15.0,    default: 0.0,    unit: "dB" },
    ParamDef { id: 7, name: b"HP",        min: 0.0,    max: 500.0,   default: 0.0,    unit: "Hz" },
    ParamDef { id: 8, name: b"LP",        min: 0.0,    max: 22000.0, default: 0.0,    unit: "Hz" },
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
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    pub dirty_params: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_begin: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub gesture_end: [std::sync::atomic::AtomicBool; PARAMS.len()],
    pub active_preset: std::sync::atomic::AtomicU32,
    /// Set whenever a param that affects the FIR moves — the audio
    /// thread checks this once per block and asks for a rebuild.
    pub fir_dirty: std::sync::atomic::AtomicBool,
    /// Audio thread → main thread: "the curve moved, design a new FIR".
    pub fir_request: std::sync::atomic::AtomicBool,
    /// Main thread → audio thread: freshly designed coefficients waiting to be
    /// swapped in. The audio thread only ever `try_lock`s this, so a main
    /// thread mid-design can never block the callback.
    pub pending_fir: parking_lot::Mutex<Option<Vec<f32>>>,
    /// Cached at activate() so the main thread can design against the host's
    /// actual rate without reaching into the audio processor.
    pub sample_rate: AtomicF32,
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
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
                active_preset: std::sync::atomic::AtomicU32::new(0),
                fir_dirty: std::sync::atomic::AtomicBool::new(true),
                fir_request: std::sync::atomic::AtomicBool::new(false),
                pending_fir: parking_lot::Mutex::new(None),
                sample_rate: AtomicF32::new(48_000.0),
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
// Audio processor
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {
    /// Woken by the audio thread's request_callback when the EQ curve moved.
    /// This is where the 2048-tap design actually happens — allocations and
    /// all — and the result is parked for the audio thread to pick up.
    fn on_main_thread(&mut self) {
        if self.shared.fir_request.swap(false, Ordering::AcqRel) {
            let sr = self.shared.sample_rate.load(Ordering::Relaxed);
            let fir = build_fir(self.shared, sr);
            *self.shared.pending_fir.lock() = Some(fir);
        }
    }
}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    host: HostAudioProcessorHandle<'a>,
    conv_l: DirectFirConvolver,
    conv_r: DirectFirConvolver,
    sample_rate: f32,
    /// Last param values the FIR was designed for. The dirty_params array
    /// cannot be used for this: emit_dirty_param_events clears it at the top
    /// of every block, so by the time the rebuild check ran, every flag was
    /// already false and host automation never triggered a redesign at all.
    last_params: [f32; PARAMS.len()],
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
}

/// Build the FIR coefficients from current param values. Samples the
/// target magnitude at FIR_LEN/2 + 1 bins by chaining the same RBJ
/// biquads the minimum-phase EQ uses, evaluating
/// `magnitude_db_at(freq)` per bin, converting to linear, then
/// designing a linear-phase FIR.
fn build_fir(shared: &PluginShared, sr: f32) -> Vec<f32> {
    let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
    // Set up biquads matching the param table — same as SuperDuper EQ.
    let mut low = Biquad::default();
    low.set_low_shelf(sr, load(P_LOW_FREQ), 0.5, load(P_LOW_GAIN));
    let mut mid = Biquad::default();
    mid.set_peaking(sr, load(P_MID_FREQ), load(P_MID_Q), load(P_MID_GAIN));
    let mut high = Biquad::default();
    high.set_high_shelf(sr, load(P_HIGH_FREQ), 0.5, load(P_HIGH_GAIN));
    let hp_hz = load(P_HP);
    let lp_hz = load(P_LP);
    let mut hp = Biquad::default();
    let mut lp = Biquad::default();
    if hp_hz > 5.0 {
        hp.set_hpf(sr, hp_hz, 0.707);
    }
    if lp_hz > 100.0 && lp_hz < sr * 0.49 {
        lp.set_lpf(sr, lp_hz, 0.707);
    }
    // Sample target magnitude.
    let n_bins = FIR_LEN / 2 + 1;
    let nyq = sr * 0.5;
    let mut target = Vec::with_capacity(n_bins);
    let out_lin = 10f32.powf(load(P_OUTPUT) / 20.0);
    for k in 0..n_bins {
        let f = (k as f32 / (n_bins - 1) as f32) * nyq;
        let f = f.max(1.0); // avoid log(0)
        let mut db = low.magnitude_db_at(f, sr)
            + mid.magnitude_db_at(f, sr)
            + high.magnitude_db_at(f, sr);
        if hp_hz > 5.0 {
            db += hp.magnitude_db_at(f, sr);
        }
        if lp_hz > 100.0 && lp_hz < sr * 0.49 {
            db += lp.magnitude_db_at(f, sr);
        }
        let lin = 10f32.powf(db / 20.0) * out_lin;
        target.push(lin);
    }
    design_linear_phase_fir(&target, FIR_LEN)
}

impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, PluginShared, PluginMainThread<'a>>
    for PluginAudioProcessor<'a>
{
    fn activate(
        host: HostAudioProcessorHandle<'a>,
        _main_thread: &mut PluginMainThread<'a>,
        shared: &'a PluginShared,
        cfg: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        let sr = cfg.sample_rate as f32;
        slog!("activate sr={}", sr);
        let fir = build_fir(shared, sr);
        let conv_l = DirectFirConvolver::new(fir.clone());
        let conv_r = DirectFirConvolver::new(fir);
        shared.fir_dirty.store(false, Ordering::Release);
        shared.sample_rate.store(sr, Ordering::Relaxed);
        Ok(Self {
            shared,
            host,
            last_params: std::array::from_fn(|i| shared.params[i].load(Ordering::Relaxed)),
            conv_l,
            conv_r,
            sample_rate: sr,
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
            &self.shared.gesture_begin, &self.shared.gesture_end, events.output);

        // Designing the FIR means an FFT plan, three Vec allocations and two
        // deallocations — ~30 ms by this crate's own estimate, against a 2.7 ms
        // block budget at 128 frames. It used to run right here, so dragging a
        // band gain dropped audio for the length of the drag. Now the audio
        // thread only notices the change and asks; the main thread designs.
        let mut curve_moved = self.shared.fir_dirty.swap(false, Ordering::AcqRel);
        for (i, last) in self.last_params.iter_mut().enumerate() {
            let now = self.shared.params[i].load(Ordering::Relaxed);
            if *last != now {
                *last = now;
                curve_moved = true;
            }
        }
        if curve_moved {
            self.shared.fir_request.store(true, Ordering::Release);
            self.host.shared().request_callback();
        }
        // try_lock, never lock: the main thread holds this while designing, and
        // waiting on it would hand the audio callback's deadline to a normal
        // priority thread.
        if let Some(mut slot) = self.shared.pending_fir.try_lock() {
            if let Some(fir) = slot.take() {
                self.conv_l.replace_fir(fir.clone());
                self.conv_r.replace_fir(fir);
            }
        }

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            let mut iter = channel_pairs.into_iter();
            let Some(ch_l) = iter.next() else { continue };
            let ch_r = iter.next();
            use superduper_dsp_sdk::clap_helpers::split_io;
            let Some((l_read, l_write)) = split_io(ch_l) else { continue };
            let r = ch_r.and_then(split_io);
            if bypassed {
                l_write.copy_from_slice(l_read);
                if let Some((r_read, r_write)) = r {
                    r_write.copy_from_slice(r_read);
                }
                continue;
            }
            match r {
                Some((r_read, r_write)) => {
                    let n = l_read.len().min(r_read.len());
                    for i in 0..n {
                        let lo = self.conv_l.process(l_read[i]);
                        let ro = self.conv_r.process(r_read[i]);
                        l_write[i] = lo;
                        r_write[i] = ro;
                        self.shared.scope.push((lo + ro) * 0.5);
                    }
                }
                None => {
                    for i in 0..l_read.len() {
                        let s = self.conv_l.process(l_read[i]);
                        l_write[i] = s;
                        self.shared.scope.push(s);
                    }
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
            // No in-place — see the note in the sibling plugins: it existed
            // only to let split_io alias one buffer as both input and output.
            in_place_pair: None,
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
        self.shared.fir_dirty.store(true, Ordering::Release);
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        apply_param_events(self.shared, ev);
        self.shared.fir_dirty.store(true, Ordering::Release);
    }
}

impl PluginLatencyImpl for PluginMainThread<'_> {
    fn get(&mut self) -> u32 {
        // Group delay of a symmetric linear-phase FIR = FIR_LEN/2.
        (FIR_LEN / 2) as u32
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
        if c.is_floating { return false; }
        c.api_type == GuiApiType::COCOA || c.api_type == GuiApiType::WIN32 || c.api_type == GuiApiType::X11
    }
    fn get_preferred_api(&mut self) -> Option<GuiConfiguration<'_>> {
        let api_type = if cfg!(target_os = "macos") { GuiApiType::COCOA }
            else if cfg!(target_os = "windows") { GuiApiType::WIN32 } else { GuiApiType::X11 };
        Some(GuiConfiguration { api_type, is_floating: false })
    }
    fn create(&mut self, _: GuiConfiguration) -> Result<(), PluginError> { Ok(()) }
    fn destroy(&mut self) { self.gui_handle = None; }
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

pub struct SuperDuperLinEq;

impl Plugin for SuperDuperLinEq {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<PluginLatency>()
            .register::<clack_extensions::gui::PluginGui>();
    }
}

impl DefaultPluginFactory for SuperDuperLinEq {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.lineq",
            plugin_display_name!("SuperDuper LinEq"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Linear-phase 3-band mastering EQ via FIR convolution")
        .with_features([AUDIO_EFFECT, STEREO, EQUALIZER, MASTERING])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: LinEq — build {} ({})", build_num!(), build_date!());
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

clack_export_entry!(SinglePluginEntry<SuperDuperLinEq>);
