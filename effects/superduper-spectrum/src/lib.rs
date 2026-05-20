//! SuperDuper Spectrum — pass-through audio analyzer.
//!
//! Routes input straight to output (no DSP modification) and pushes the
//! audio buffer into a lock-free SPSC ring buffer that the GUI thread reads
//! to draw a live spectrum. Will become the visualization layer for the
//! upcoming SuperDuper EQ when DSP knobs land.
//!
//! Three knobs:
//!   - **FFT Size** — 1024 / 2048 / 4096 / 8192 (stepped)
//!   - **Smoothing** — 0..1 (one-pole on each bin, 0 = none, 1 = frozen)
//!   - **Tilt** — visual only, slope in dB/octave, like FabFilter Pro-Q's
//!     reference line (gives "pink-noise = flat" feel)

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod palette;
pub mod ring;

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

// ---------------------------------------------------------------------------
// Per-plugin file log (stderr is swallowed on Dock-launched plugin hosts).
// ---------------------------------------------------------------------------

fn init_logging() { superduper_dsp_sdk::log::init("spectrum"); }
use superduper_dsp_sdk::slog;

// ---------------------------------------------------------------------------
// Parameter table
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"FFT Size",  min: 1024.0, max: 8192.0, default: 2048.0, unit: "" },
    ParamDef { id: 1, name: b"Smoothing", min: 0.0,    max: 1.0,    default: 0.7,    unit: "" },
    ParamDef { id: 2, name: b"Tilt",      min: -6.0,   max: 6.0,    default: 4.5,    unit: "dB/oct" },
    // 0 = Spectrum (line), 1 = Spectrogram (waterfall), 2 = Split view
    ParamDef { id: 3, name: b"Mode",      min: 0.0,    max: 2.0,    default: 0.0,    unit: "" },
    // 0 = Phosphor (green), 1 = Heat (blue→red), 2 = Mono (grey)
    ParamDef { id: 4, name: b"Palette",   min: 0.0,    max: 2.0,    default: 0.0,    unit: "" },
    // Spectrogram time window in seconds (controls scroll speed).
    ParamDef { id: 5, name: b"Window",    min: 0.5,    max: 30.0,   default: 5.0,    unit: "s" },
];

pub const P_FFT_SIZE: usize = 0;
pub const P_SMOOTHING: usize = 1;
pub const P_TILT: usize = 2;
pub const P_MODE: usize = 3;
pub const P_PALETTE: usize = 4;
pub const P_WINDOW: usize = 5;

// ---------------------------------------------------------------------------
// SharedParams — same Arc pattern used by reverb / supermass for GUI access.
// ---------------------------------------------------------------------------

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: std::sync::atomic::AtomicBool,
    pub ab_snapshot: superduper_synth_core::gui::AbSnapshot,
    pub scope: superduper_synth_core::gui::LiveScope,
    /// Currently-selected preset index — persisted via simple_state.
    pub active_preset: std::sync::atomic::AtomicU32,
    /// Latest LUFS / dBTP readings — audio thread updates whenever a
    /// 100 ms block boundary rolls over; GUI samples at ~60 Hz.
    pub lufs_momentary: AtomicF32,
    pub lufs_short_term: AtomicF32,
    pub lufs_integrated: AtomicF32,
    pub true_peak_dbtp: AtomicF32,
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
                ab_snapshot: superduper_synth_core::gui::AbSnapshot::new(PARAMS.len()),
                scope: superduper_synth_core::gui::LiveScope::new(1024),
                active_preset: std::sync::atomic::AtomicU32::new(0),
                lufs_momentary: AtomicF32::new(-100.0),
                lufs_short_term: AtomicF32::new(-100.0),
                lufs_integrated: AtomicF32::new(-100.0),
                true_peak_dbtp: AtomicF32::new(f32::NEG_INFINITY),
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

// ---------------------------------------------------------------------------
// Main thread + audio processor
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    /// Receiver half of the audio → GUI ring buffer. Stored here so the GUI
    /// thread can pick it up at `set_parent` time without needing access to
    /// the audio processor.
    consumer: parking_lot::Mutex<Option<rtrb::Consumer<f32>>>,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}

impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    /// Producer half of the ring buffer (audio side).
    producer: Option<rtrb::Producer<f32>>,
    sample_rate: f32,
    /// BS.1770 K-weighted loudness meter — fed from the stereo input
    /// per sample; readings published to shared atomics on every
    /// 100 ms block boundary so the GUI can poll cheaply.
    loudness: superduper_synth_core::loudness::LoudnessMeter,
    true_peak: superduper_synth_core::loudness::TruePeakDetector,
}

fn apply_param_events(shared: &PluginShared, events: &InputEvents) {
    superduper_dsp_sdk::clap_helpers::apply_param_events(&shared.params, events);
}

impl<'a> clack_plugin::plugin::PluginAudioProcessor<'a, PluginShared, PluginMainThread<'a>>
    for PluginAudioProcessor<'a>
{
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        main_thread: &mut PluginMainThread<'a>,
        shared: &'a PluginShared,
        audio_config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        slog!(
            "activate: sr={}, frames=..{}",
            audio_config.sample_rate, audio_config.max_frames_count
        );
        // Ring buffer sized for ~4 max-blocks. Audio thread overwrites stale
        // data if GUI gets behind, which is what we want — we draw "latest".
        let cap = (audio_config.max_frames_count as usize * 4).max(8192);
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(cap);
        *main_thread.consumer.lock() = Some(consumer);
        let sr = audio_config.sample_rate as f32;
        Ok(Self {
            shared,
            producer: Some(producer),
            sample_rate: sr,
            loudness: superduper_synth_core::loudness::LoudnessMeter::new(sr),
            true_peak: superduper_synth_core::loudness::TruePeakDetector::new(),
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        apply_param_events(self.shared, events.input);

        let bypassed = self.shared.bypass.load(Ordering::Relaxed);
        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
            // Collect L (and optional R) slices first so the loudness
            // meter sees true stereo. The spectrum visualiser then
            // pushes L into its own ring.
            let mut slices: Vec<&[f32]> = Vec::with_capacity(2);
            for channel_pair in channel_pairs {
                match channel_pair {
                    ChannelPair::InputOutput(input, output) => {
                        output.copy_from_slice(input);
                        slices.push(input);
                    }
                    ChannelPair::InPlace(buf) => slices.push(buf),
                    ChannelPair::OutputOnly(buf) => buf.fill(0.0),
                    ChannelPair::InputOnly(input) => slices.push(input),
                }
            }
            if !bypassed {
                if let Some(left) = slices.first().copied() {
                    let right = slices.get(1).copied().unwrap_or(left);
                    let mut block_rolled_over = false;
                    let n = left.len().min(right.len());
                    for i in 0..n {
                        let l = left[i];
                        let r = right[i];
                        self.true_peak.process_stereo(l, r);
                        if self.loudness.process_stereo(l, r) {
                            block_rolled_over = true;
                        }
                    }
                    // Publish on 100 ms boundary — GUI reads at 60 Hz
                    // anyway, no need to hit the atomic every sample.
                    if block_rolled_over {
                        self.shared
                            .lufs_momentary
                            .store(self.loudness.momentary_lufs(), Ordering::Relaxed);
                        self.shared
                            .lufs_short_term
                            .store(self.loudness.short_term_lufs(), Ordering::Relaxed);
                        self.shared
                            .lufs_integrated
                            .store(self.loudness.integrated_lufs(), Ordering::Relaxed);
                        self.shared
                            .true_peak_dbtp
                            .store(self.true_peak.dbtp(), Ordering::Relaxed);
                    }
                    Self::push_to_ring(&mut self.producer, left);
                }
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

impl PluginAudioProcessor<'_> {
    /// Mix the channel into the ring buffer. We only push the first channel
    /// (L) for now — the spectrum view is mono.
    #[inline]
    fn push_to_ring(producer: &mut Option<rtrb::Producer<f32>>, samples: &[f32]) {
        let Some(p) = producer.as_mut() else { return };
        for &s in samples {
            // try_push fails when full — we just drop newest samples and let
            // the GUI catch up. No locking, no realloc, no blocking.
            if p.push(s).is_err() {
                // Drain one slot to make room (overwrite-oldest behavior).
                // Doing it via the producer's len() check vs available slots
                // would race with the consumer; the cleanest move is to bail.
                break;
            }
        }
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
    fn get_info(&mut self, param_index: u32, info: &mut ParamInfoWriter) {
        ParamDef::write_info(PARAMS, param_index, info);
    }
    fn get_value(&mut self, param_id: ClapId) -> Option<f64> {
        let i = param_id.get() as usize;
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
        let shared = self.shared.shared_handle();
        let consumer = self.consumer.lock().take();
        let handle = gui::open_window(&window, shared, consumer, self.gui_resize.clone());
        self.gui_handle = Some(handle);
        Ok(())
    }
    fn set_transient(&mut self, _: ClapGuiWindow) -> Result<(), PluginError> { Ok(()) }
    fn show(&mut self) -> Result<(), PluginError> { slog!("gui::show"); Ok(()) }
    fn hide(&mut self) -> Result<(), PluginError> { slog!("gui::hide"); Ok(()) }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

pub struct SuperDuperSpectrum;

impl Plugin for SuperDuperSpectrum {
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

impl DefaultPluginFactory for SuperDuperSpectrum {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.spectrum",
            plugin_display_name!("SuperDuper Spectrum"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Real-time spectrum analyzer (pass-through)")
        .with_features([ANALYZER, AUDIO_EFFECT, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
        init_logging();
        slog!("new_shared: Spectrum — build {} ({})", build_num!(), build_date!());
        Ok(PluginShared::new())
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        shared: &'a PluginShared,
    ) -> Result<PluginMainThread<'a>, PluginError> {
        Ok(PluginMainThread {
            shared,
            consumer: parking_lot::Mutex::new(None),
            gui_handle: None,
            gui_resize: gui::new_resize_bridge(),
        })
    }
}

clack_export_entry!(SinglePluginEntry<SuperDuperSpectrum>);
