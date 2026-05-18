//! SuperDuper Kubyz — physical-model jaw-harp synth.
//!
//! Architecture mirrors Pad/Wave: 8-voice pool, sample-accurate batching,
//! click-free voice steal, soft-fade choke. DSP is additive-16-harmonics
//! plus a 3-band formant (matches KubizBeat's KubyzVoice.swift).
//!
//! Harmonic amplitudes are kept in a `Mutex<[f32; N_HARMONICS]>` on the
//! Shared so the GUI can let the user draw them live. Presets ship as
//! Rust consts (see `presets.rs`).

#![allow(clippy::missing_safety_doc)]

pub mod gui;
pub mod presets;
pub mod voice;

use atomic_float::AtomicF32;
use clack_common::events::Match;
use clack_common::events::spaces::CoreEventSpace;
use clack_common::utils::ClapId;
use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::note_ports::{
    NoteDialect, NoteDialects, NotePortInfo, NotePortInfoWriter, PluginNotePorts,
    PluginNotePortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfoWriter, PluginAudioProcessorParams, PluginMainThreadParams,
    PluginParams,
};
use clack_plugin::plugin::features::*;
use clack_plugin::prelude::*;
use parking_lot::Mutex;
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use superduper_dsp_sdk::clap_helpers::ParamDef;
use superduper_dsp_sdk::{build_date, build_num, plugin_display_name, version_string};
use superduper_synth_core::dsp_blocks::{AdsrEnvelope, AdsrParams, SmoothedParam, midi_note_to_hz};

use presets::{presets, KubyzPreset, N_HARMONICS};
use voice::{KubyzParams, KubyzVoice, NOTE_FREE};

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

pub const PARAMS: &[ParamDef] = &[
    ParamDef { id: 0, name: b"F1",        min: 80.0,   max: 1500.0, default: 705.0,  unit: "Hz" },
    ParamDef { id: 1, name: b"F2",        min: 200.0,  max: 3500.0, default: 1301.0, unit: "Hz" },
    ParamDef { id: 2, name: b"F3",        min: 600.0,  max: 6000.0, default: 2165.0, unit: "Hz" },
    ParamDef { id: 3, name: b"VoxMix",    min: 0.0,    max: 1.0,    default: 0.6,    unit: ""   },
    ParamDef { id: 4, name: b"Vel Shift", min: 0.0,    max: 0.5,    default: 0.15,   unit: ""   },
    ParamDef { id: 5, name: b"Bright",    min: 0.0,    max: 2.0,    default: 1.0,    unit: ""   },
    ParamDef { id: 6, name: b"Attack",    min: 0.001,  max: 2.0,    default: 0.039,  unit: "s"  },
    ParamDef { id: 7, name: b"Decay",     min: 0.01,   max: 4.0,    default: 0.21,   unit: "s"  },
    ParamDef { id: 8, name: b"Sustain",   min: 0.0,    max: 1.0,    default: 0.13,   unit: ""   },
    ParamDef { id: 9, name: b"Release",   min: 0.01,   max: 4.0,    default: 0.15,   unit: "s"  },
    ParamDef { id: 10, name: b"Output",   min: -36.0,  max: 6.0,    default: -8.0,   unit: "dB" },
];

pub const P_F1: usize = 0;
pub const P_F2: usize = 1;
pub const P_F3: usize = 2;
pub const P_VOX_MIX: usize = 3;
pub const P_VEL_SHIFT: usize = 4;
pub const P_BRIGHT: usize = 5;
pub const P_ATTACK: usize = 6;
pub const P_DECAY: usize = 7;
pub const P_SUSTAIN: usize = 8;
pub const P_RELEASE: usize = 9;
pub const P_OUTPUT: usize = 10;

pub const VOICE_COUNT: usize = 8;

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

pub type SharedParams = std::sync::Arc<SharedParamsInner>;

pub struct SharedParamsInner {
    pub params: [AtomicF32; PARAMS.len()],
    pub bypass: AtomicBool,
    pub active_voices: AtomicU32,
    /// Active 16-harmonic amplitude table. Atomic per slot so the GUI's
    /// harmonic-bar editor can scribble into it without a lock and the
    /// audio thread can read directly per render call.
    pub harmonics: [AtomicF32; N_HARMONICS],
    pub formant_bw: Mutex<[f32; 3]>,
    pub formant_gain: Mutex<[f32; 3]>,
}

pub struct PluginShared {
    pub inner: SharedParams,
}

impl PluginShared {
    pub fn new() -> Self {
        let init = &presets()[0];
        let harmonics = std::array::from_fn(|i| AtomicF32::new(init.harmonics[i]));
        Self {
            inner: std::sync::Arc::new(SharedParamsInner {
                params: std::array::from_fn(|i| AtomicF32::new(PARAMS[i].default as f32)),
                bypass: AtomicBool::new(false),
                active_voices: AtomicU32::new(0),
                harmonics,
                formant_bw: Mutex::new(init.formant.bw),
                formant_gain: Mutex::new(init.formant.gain),
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

/// Push every field of `preset` into Shared — used by the GUI when the
/// user picks a preset.
pub fn apply_preset(shared: &SharedParamsInner, preset: &KubyzPreset) {
    shared.params[P_F1].store(preset.formant.f[0], Ordering::Relaxed);
    shared.params[P_F2].store(preset.formant.f[1], Ordering::Relaxed);
    shared.params[P_F3].store(preset.formant.f[2], Ordering::Relaxed);
    shared.params[P_ATTACK].store(preset.attack_s, Ordering::Relaxed);
    shared.params[P_DECAY].store(preset.decay_s, Ordering::Relaxed);
    shared.params[P_SUSTAIN].store(preset.sustain, Ordering::Relaxed);
    shared.params[P_RELEASE].store(preset.release_s, Ordering::Relaxed);
    shared.params[P_VEL_SHIFT].store(preset.velocity_formant_shift, Ordering::Relaxed);
    shared.params[P_BRIGHT].store(1.0, Ordering::Relaxed);
    shared.params[P_VOX_MIX].store(preset.default_vox_mix, Ordering::Relaxed);
    for i in 0..N_HARMONICS {
        shared.harmonics[i].store(preset.harmonics[i], Ordering::Relaxed);
    }
    *shared.formant_bw.lock() = preset.formant.bw;
    *shared.formant_gain.lock() = preset.formant.gain;
}

// ---------------------------------------------------------------------------
// Main-thread + audio processor
// ---------------------------------------------------------------------------

pub struct PluginMainThread<'a> {
    shared: &'a PluginShared,
    gui_handle: Option<baseview::WindowHandle>,
    gui_resize: gui::ResizeBridge,
}
impl<'a> clack_plugin::plugin::PluginMainThread<'a, PluginShared> for PluginMainThread<'a> {}

pub struct PluginAudioProcessor<'a> {
    shared: &'a PluginShared,
    voices: [KubyzVoice; VOICE_COUNT],
    next_age: u64,
    smooth_f1: SmoothedParam,
    smooth_f2: SmoothedParam,
    smooth_f3: SmoothedParam,
    smooth_vox: SmoothedParam,
    smooth_bright: SmoothedParam,
    smooth_output: SmoothedParam,
    sample_rate: f32,
}

#[inline]
fn matches_key(target: Match<u16>, key: u8) -> bool {
    match target {
        Match::All => true,
        Match::Specific(k) => k as u8 == key,
    }
}
#[inline]
fn matches_note_id(target: Match<u32>, note_id: i32) -> bool {
    match target {
        Match::All => true,
        Match::Specific(id) => id as i32 == note_id,
    }
}

impl<'a> PluginAudioProcessor<'a> {
    fn allocate_voice(&mut self, key: u8, velocity: f32, note_id: i32) {
        self.next_age = self.next_age.wrapping_add(1);
        let stamp = self.next_age;
        // 1. Retrigger
        for v in self.voices.iter_mut() {
            if v.key == key && v.note_id == note_id {
                v.env.gate_on();
                v.velocity = velocity;
                v.age_stamp = stamp;
                v.choke_remaining = 0;
                return;
            }
        }
        // 2. Free
        if let Some(v) = self.voices.iter_mut().find(|v| v.env.is_idle() && v.choke_remaining == 0) {
            v.key = key;
            v.note_id = note_id;
            v.velocity = velocity;
            v.age_stamp = stamp;
            v.env = AdsrEnvelope::default();
            v.env.gate_on();
            v.choke_remaining = 0;
            return;
        }
        // 3. Steal — quietest releasing, else oldest.
        let mut steal_idx = 0usize;
        let mut steal_score = f32::INFINITY;
        let mut found_release = false;
        for (i, v) in self.voices.iter().enumerate() {
            if v.env.is_releasing() {
                let lvl = v.env.level();
                if lvl < steal_score {
                    steal_score = lvl;
                    steal_idx = i;
                    found_release = true;
                }
            }
        }
        if !found_release {
            let mut oldest = u64::MAX;
            for (i, v) in self.voices.iter().enumerate() {
                if v.age_stamp < oldest {
                    oldest = v.age_stamp;
                    steal_idx = i;
                }
            }
        }
        let v = &mut self.voices[steal_idx];
        v.key = key;
        v.note_id = note_id;
        v.velocity = velocity;
        v.age_stamp = stamp;
        v.choke_remaining = 0;
        v.env.gate_on();
    }

    fn release_voice(&mut self, key_match: Match<u16>, note_id_match: Match<u32>) {
        for v in self.voices.iter_mut() {
            if v.key == NOTE_FREE { continue; }
            if matches_key(key_match, v.key) && matches_note_id(note_id_match, v.note_id) {
                v.env.gate_off();
            }
        }
    }

    fn choke_voice(&mut self, key_match: Match<u16>, note_id_match: Match<u32>) {
        let fade_samples = (self.sample_rate * 0.005) as u32;
        for v in self.voices.iter_mut() {
            if v.key == NOTE_FREE && v.choke_remaining == 0 { continue; }
            if matches_key(key_match, v.key) && matches_note_id(note_id_match, v.note_id) {
                v.choke_level = v.env.level();
                v.choke_total = fade_samples.max(1);
                v.choke_remaining = v.choke_total;
            }
        }
    }

    fn handle_midi_event(&mut self, data: [u8; 3]) {
        let status = data[0] & 0xf0;
        let key = data[1];
        let raw_velocity = data[2];
        match status {
            0x90 => {
                if raw_velocity == 0 {
                    self.release_voice(Match::Specific(key as u16), Match::All);
                } else {
                    self.allocate_voice(key, raw_velocity as f32 / 127.0, -1);
                }
            }
            0x80 => self.release_voice(Match::Specific(key as u16), Match::All),
            0xb0 if key == 123 => self.release_voice(Match::All, Match::All),
            0xb0 if key == 120 => self.choke_voice(Match::All, Match::All),
            _ => {}
        }
    }

    fn handle_note_event(&mut self, ev: &CoreEventSpace<'_>) {
        match ev {
            CoreEventSpace::NoteOn(n) => {
                let key = match n.key() {
                    Match::Specific(k) => k as u8,
                    Match::All => return,
                };
                let note_id = match n.note_id() {
                    Match::Specific(id) => id as i32,
                    Match::All => -1,
                };
                self.allocate_voice(key, n.velocity().clamp(0.0, 1.0) as f32, note_id);
            }
            CoreEventSpace::NoteOff(n) => self.release_voice(n.key(), n.note_id()),
            CoreEventSpace::NoteChoke(n) => self.choke_voice(n.key(), n.note_id()),
            CoreEventSpace::Midi(m) => self.handle_midi_event(m.data()),
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_subblock(
        &mut self,
        out_l: &mut [f32],
        out_r: &mut [f32],
        f1_target: f32, f2_target: f32, f3_target: f32,
        vox_target: f32, bright_target: f32, output_target: f32,
        attack_s: f32, decay_s: f32, sustain: f32, release_s: f32,
        vel_shift: f32,
        formant_bw: [f32; 3], formant_gain: [f32; 3],
    ) {
        let sr = self.sample_rate;
        // Snapshot current harmonic table and scale by Bright.
        let mut harmonics = [0.0_f32; N_HARMONICS];
        for i in 0..N_HARMONICS {
            harmonics[i] = self.shared.harmonics[i].load(Ordering::Relaxed);
        }

        for i in 0..out_l.len() {
            let f1 = self.smooth_f1.step(f1_target, sr);
            let f2 = self.smooth_f2.step(f2_target, sr);
            let f3 = self.smooth_f3.step(f3_target, sr);
            let vox = self.smooth_vox.step(vox_target, sr).clamp(0.0, 1.0);
            let bright = self.smooth_bright.step(bright_target, sr).max(0.0);
            let output_db = self.smooth_output.step(output_target, sr);

            // Apply brightness: bright > 1 emphasises higher harmonics
            // (multiplies harmonic n by `bright^((n-1)/4)`), <1 mutes.
            let mut h_scaled = [0.0_f32; N_HARMONICS];
            for (n, slot) in h_scaled.iter_mut().enumerate() {
                let exp = (n as f32) / 4.0;
                *slot = harmonics[n] * bright.powf(exp);
            }

            let adsr = AdsrParams { sr, attack_s, decay_s, sustain, release_s };
            let mut mix_l = 0.0_f32;
            let mut mix_r = 0.0_f32;
            for v in self.voices.iter_mut() {
                if v.key == NOTE_FREE && v.env.is_idle() && v.choke_remaining == 0 {
                    continue;
                }
                let root = midi_note_to_hz(v.key as f32);
                let params = KubyzParams {
                    sr,
                    root_hz: root,
                    harmonics: &h_scaled,
                    formant_f: [f1, f2, f3],
                    formant_bw,
                    formant_gain,
                    formant_mix: vox,
                    velocity_formant_shift: vel_shift,
                };
                if v.choke_remaining > 0 {
                    let fade = (v.choke_remaining as f32) / (v.choke_total as f32);
                    let (l, r) = v.process(params);
                    let amp = fade * v.choke_level * v.velocity;
                    mix_l += l * amp;
                    mix_r += r * amp;
                    v.choke_remaining -= 1;
                    if v.choke_remaining == 0 {
                        v.env = AdsrEnvelope::default();
                        v.key = NOTE_FREE;
                    }
                    continue;
                }
                let env = v.env.process(adsr);
                if env <= 1e-5 && v.env.is_idle() {
                    v.key = NOTE_FREE;
                    continue;
                }
                let (l, r) = v.process(params);
                let amp = env * v.velocity;
                mix_l += l * amp;
                mix_r += r * amp;
            }
            let voice_scale = 0.5_f32;
            let out_lin = 10f32.powf(output_db / 20.0);
            out_l[i] = mix_l * voice_scale * out_lin;
            out_r[i] = mix_r * voice_scale * out_lin;
        }
    }

    fn count_active(&self) -> u32 {
        self.voices
            .iter()
            .filter(|v| !v.env.is_idle() || v.key != NOTE_FREE || v.choke_remaining > 0)
            .count() as u32
    }
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
        let load = |i: usize| shared.params[i].load(Ordering::Relaxed);
        Ok(Self {
            shared,
            voices: std::array::from_fn(|_| KubyzVoice::default()),
            next_age: 0,
            smooth_f1: SmoothedParam::new(load(P_F1)),
            smooth_f2: SmoothedParam::new(load(P_F2)),
            smooth_f3: SmoothedParam::new(load(P_F3)),
            smooth_vox: SmoothedParam::new(load(P_VOX_MIX)),
            smooth_bright: SmoothedParam::new(load(P_BRIGHT)),
            smooth_output: SmoothedParam::new(load(P_OUTPUT)),
            sample_rate: sr,
        })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let bypassed = self.shared.bypass.load(Ordering::Relaxed);

        for mut port_pair in &mut audio {
            let Some(channel_pairs) = port_pair.channels()?.into_f32() else {
                continue;
            };
            let mut writers: Vec<_> = channel_pairs
                .into_iter()
                .filter_map(superduper_dsp_sdk::clap_helpers::output_slice)
                .collect();
            if writers.len() < 2 {
                for w in writers.iter_mut() { w.fill(0.0); }
                continue;
            }
            let (a, b) = writers.split_at_mut(1);
            let out_l: &mut [f32] = a[0];
            let out_r: &mut [f32] = b[0];
            let frames = out_l.len().min(out_r.len());
            if bypassed {
                out_l[..frames].fill(0.0);
                out_r[..frames].fill(0.0);
                continue;
            }

            for batch in events.input.batch() {
                for ev in batch.events() {
                    if let Some(core) = ev.as_core_event() {
                        match core {
                            CoreEventSpace::ParamValue(pv) => {
                                if let Some(id) = pv.param_id() {
                                    let idx = id.get() as usize;
                                    if let Some(atom) = self.shared.params.get(idx) {
                                        atom.store(pv.value() as f32, Ordering::Relaxed);
                                    }
                                }
                            }
                            _ => self.handle_note_event(&core),
                        }
                    }
                }
                let start = batch.first_sample().min(frames);
                let end = batch.next_batch_first_sample().unwrap_or(frames).min(frames);
                if end <= start { continue; }

                let f1 = self.shared.params[P_F1].load(Ordering::Relaxed);
                let f2 = self.shared.params[P_F2].load(Ordering::Relaxed);
                let f3 = self.shared.params[P_F3].load(Ordering::Relaxed);
                let vox = self.shared.params[P_VOX_MIX].load(Ordering::Relaxed);
                let bright = self.shared.params[P_BRIGHT].load(Ordering::Relaxed);
                let output = self.shared.params[P_OUTPUT].load(Ordering::Relaxed);
                let attack = self.shared.params[P_ATTACK].load(Ordering::Relaxed);
                let decay = self.shared.params[P_DECAY].load(Ordering::Relaxed);
                let sustain = self.shared.params[P_SUSTAIN].load(Ordering::Relaxed);
                let release = self.shared.params[P_RELEASE].load(Ordering::Relaxed);
                let vel_shift = self.shared.params[P_VEL_SHIFT].load(Ordering::Relaxed);
                let formant_bw = *self.shared.formant_bw.lock();
                let formant_gain = *self.shared.formant_gain.lock();

                self.render_subblock(
                    &mut out_l[start..end],
                    &mut out_r[start..end],
                    f1, f2, f3, vox, bright, output,
                    attack, decay, sustain, release,
                    vel_shift,
                    formant_bw, formant_gain,
                );
            }
            if writers.len() > 2 {
                for w in writers.iter_mut().skip(2) { w.fill(0.0); }
            }
        }
        self.shared.active_voices.store(self.count_active(), Ordering::Relaxed);
        Ok(ProcessStatus::Continue)
    }
}

// ---------------------------------------------------------------------------
// CLAP extensions
// ---------------------------------------------------------------------------

impl PluginAudioPortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 { if is_input { 0 } else { 1 } }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if is_input || index != 0 { return; }
        writer.set(&AudioPortInfo {
            id: ClapId::new(0),
            name: b"Output",
            channel_count: 2,
            flags: AudioPortFlags::IS_MAIN,
            port_type: Some(AudioPortType::STEREO),
            in_place_pair: None,
        });
    }
}

impl PluginNotePortsImpl for PluginMainThread<'_> {
    fn count(&mut self, is_input: bool) -> u32 { if is_input { 1 } else { 0 } }
    fn get(&mut self, index: u32, is_input: bool, writer: &mut NotePortInfoWriter) {
        if !is_input || index != 0 { return; }
        writer.set(&NotePortInfo {
            id: ClapId::new(0),
            name: b"Notes",
            supported_dialects: NoteDialects::CLAP | NoteDialects::MIDI,
            preferred_dialect: Some(NoteDialect::Clap),
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
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
    }
}

impl PluginAudioProcessorParams for PluginAudioProcessor<'_> {
    fn flush(&mut self, ev: &InputEvents, _out: &mut OutputEvents) {
        superduper_dsp_sdk::clap_helpers::apply_param_events(&self.shared.params, ev);
    }
}

use clack_extensions::gui::{
    AspectRatioStrategy, GuiApiType, GuiConfiguration, GuiResizeHints, GuiSize, PluginGuiImpl,
    Window as ClapGuiWindow,
};

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
            width: self.gui_resize.0.load(Ordering::Relaxed),
            height: self.gui_resize.1.load(Ordering::Relaxed),
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
        self.gui_resize.0.store(w, Ordering::Relaxed);
        self.gui_resize.1.store(h, Ordering::Relaxed);
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

pub struct SuperDuperKubyz;

impl Plugin for SuperDuperKubyz {
    type AudioProcessor<'a> = PluginAudioProcessor<'a>;
    type Shared<'a> = PluginShared;
    type MainThread<'a> = PluginMainThread<'a>;
    fn declare_extensions(builder: &mut PluginExtensions<Self>, _: Option<&Self::Shared<'_>>) {
        builder
            .register::<PluginAudioPorts>()
            .register::<PluginNotePorts>()
            .register::<PluginParams>()
            .register::<clack_extensions::gui::PluginGui>();
    }
}

impl DefaultPluginFactory for SuperDuperKubyz {
    fn get_descriptor() -> PluginDescriptor {
        PluginDescriptor::new(
            "co.superduperai.kubyz",
            plugin_display_name!("SuperDuper Kubyz"),
        )
        .with_vendor("SuperDuperAI")
        .with_version(version_string!("0.1"))
        .with_description("Bashkir/Yakut jaw-harp synth — additive 16-harmonic + 3-band formant")
        .with_features([INSTRUMENT, STEREO, SYNTHESIZER])
    }
    fn new_shared(_host: HostSharedHandle<'_>) -> Result<PluginShared, PluginError> {
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

clack_export_entry!(SinglePluginEntry<SuperDuperKubyz>);

#[allow(dead_code)]
fn _meta() -> (&'static str, &'static str) {
    (build_num!(), build_date!())
}
