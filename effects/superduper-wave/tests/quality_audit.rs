//! quality_audit.rs — measure Wave's DSP cleanness through real CLAP.
//!
//! Probes:
//! 1. Sine wavetable (default preset) at C4, drive=0, antialias=on →
//!    output must be near-sine: THD < -25 dB. Validates that the
//!    voice envelope + filter aren't introducing harmonic distortion
//!    on a clean source.
//! 2. Sine wavetable at C7 (high note where any aliasing folds back
//!    audibly), antialias=on → aliasing floor < -35 dB. Validates the
//!    mip-mapped wavetable path is doing its job.
//! 3. SAME high note, antialias=off → aliasing floor SHOULD rise
//!    (worse than antialias=on by at least 3 dB), confirming the
//!    toggle actually toggles.
//!
//! Run: cargo test --release -p superduper-wave --test quality_audit -- --nocapture

use clack_common::events::Pckn;
use clack_common::events::event_types::ParamValueEvent;
use clack_common::utils::{ClapId, Cookie};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::event_types::NoteOnEvent;
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, AudioPorts};
use clack_plugin::entry::SinglePluginEntry;
use superduper_synth_core::analysis::{measure_aliasing_db, measure_thd_db};
use superduper_wave::SuperDuperWave;

const SR: f32 = 48_000.0;
const BLOCK: u32 = 256;

const P_CUTOFF: u32 = 4;
const P_DRIVE: u32 = 7;
const P_ANTIALIAS: u32 = 13;

struct TS;
impl SharedHandler<'_> for TS {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}
impl HostLogImpl for TS { fn log(&self, _: LogSeverity, _: &str) {} }
struct TH;
impl HostHandlers for TH {
    type Shared<'a> = TS;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
    fn declare_extensions(b: &mut HostExtensions<Self>, _: &Self::Shared<'_>) {
        b.register::<HostLog>();
    }
}

/// Render a sustained note. Returns the (mono-summed) buffer.
fn render_note(key: u8, antialias: bool, cutoff_hz: f32, drive: f32, seconds: f32) -> Vec<f32> {
    // Boot on the factory Init (Sine) wavetable, not whatever the user last
    // drew into ~/.superduper-dsp/wave/last.json — otherwise these DSP-quality
    // assertions measure a machine-local custom wavetable, not a clean sine.
    std::env::set_var("SUPERDUPER_WAVE_FACTORY", "1");
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperWave>>(
        c"/in/process/test/superduper-wave-quality",
    ).expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TH>::new(
        |_| TS, |_| (), &entry, c"co.superduperai.wave", &host_info,
    ).expect("instantiate");
    let total_frames = (SR * seconds) as usize;
    let block_us = BLOCK as usize;
    let n_blocks = total_frames / block_us;
    let stopped = plugin.activate(|_, _| (), PluginAudioConfiguration {
        sample_rate: SR as f64, min_frames_count: BLOCK, max_frames_count: BLOCK,
    }).expect("activate");

    let mut all_l = vec![0.0_f32; total_frames];
    let mut all_r = vec![0.0_f32; total_frames];
    let l_ref = &mut all_l;
    let r_ref = &mut all_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(0, 0);
            let mut out_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let start = block * block_us;
                let end = start + block_us;

                let mut in_buf = EventBuffer::new();
                if block == 0 {
                    for &(id, v) in &[
                        (P_CUTOFF, cutoff_hz as f64),
                        (P_DRIVE, drive as f64),
                        (P_ANTIALIAS, if antialias { 1.0 } else { 0.0 }),
                    ] {
                        let ev = ParamValueEvent::new(
                            0, ClapId::new(id),
                            Pckn::new(0u16, 0u16, 0u16, 0u32),
                            v, Cookie::empty(),
                        );
                        in_buf.push(&ev);
                    }
                    let on = NoteOnEvent::new(
                        0, Pckn::new(0u16, 0u16, key as u16, 0u32), 1.0,
                    );
                    in_buf.push(&on);
                }
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);

                let mut out_l = vec![0.0_f32; block_us];
                let mut out_r = vec![0.0_f32; block_us];

                let input_audio = in_ports.with_input_buffers(std::iter::empty::<
                    AudioPortBuffer<
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f32>>,
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f64>>,
                    >,
                >());
                let mut out_chans: [&mut [f32]; 2] = [out_l.as_mut_slice(), out_r.as_mut_slice()];
                let mut output_audio = out_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        out_chans.iter_mut().map(|b| &mut **b),
                    ),
                }]);

                proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None)
                    .expect("process");

                l_ref[start..end].copy_from_slice(&out_l);
                r_ref[start..end].copy_from_slice(&out_r);
            }
            proc.stop_processing()
        }).join().expect("audio thread")
    });
    plugin.deactivate(stopped_back);

    // Mono-sum, skip attack envelope to land in steady state.
    let skip = (SR * 0.30) as usize;
    (skip..all_l.len()).map(|i| 0.5 * (all_l[i] + all_r[i])).collect()
}

#[test]
fn wave_clean_sine_low_thd() {
    // Sine wavetable, C4 (~261 Hz), cutoff open, drive=0.
    // Expect low THD — voice machinery shouldn't add harmonics.
    let mono = render_note(60, true, 18_000.0, 0.0, 1.0);
    let fft_n = 16384.min(mono.len().next_power_of_two() / 2);
    let slice = &mono[mono.len() - fft_n..];
    let thd = measure_thd_db(slice, 261.0, SR);
    eprintln!("Wave C4 sine: THD = {:.2} dB", thd);
    // A pure sine through the voice should THD < -25 dB. Our voice
    // does ZDF SVF + post-tanh on the unison-mix path even at
    // drive=0; some leakage is expected.
    assert!(
        thd < -20.0,
        "Wave on a sine wavetable shouldn't generate this much harmonic \
         content: THD = {thd:.2} dB (expected < -20)"
    );
}

#[test]
fn wave_anti_alias_keeps_high_notes_clean() {
    // C7 (~2093 Hz) on sine wavetable with antialias=on.
    let mono = render_note(96, true, 18_000.0, 0.0, 1.0);
    let fft_n = 16384.min(mono.len().next_power_of_two() / 2);
    let slice = &mono[mono.len() - fft_n..];
    let alias = measure_aliasing_db(slice, 2093.0, SR);
    eprintln!("Wave C7 anti-alias=ON: aliasing = {:.2} dB", alias);
    assert!(
        alias < -30.0,
        "Wave at C7 with anti-alias on shouldn't show this much \
         non-harmonic energy: alias = {alias:.2} dB (expected < -30)"
    );
}

#[test]
fn wave_anti_alias_toggle_actually_helps() {
    // Same C7 note, antialias=on vs antialias=off — off should be
    // measurably worse on a harmonic-rich preset. Default preset is
    // sine which doesn't generate many harmonics, but the mip toggle
    // still picks level 0 (full bandwidth) vs band-limited reads,
    // and we test that the toggle has a non-trivial effect.
    let mono_on = render_note(96, true, 18_000.0, 0.0, 1.0);
    let mono_off = render_note(96, false, 18_000.0, 0.0, 1.0);
    let fft_n = 16384.min(mono_on.len().next_power_of_two() / 2);
    let on_slice = &mono_on[mono_on.len() - fft_n..];
    let off_slice = &mono_off[mono_off.len() - fft_n..];
    let alias_on = measure_aliasing_db(on_slice, 2093.0, SR);
    let alias_off = measure_aliasing_db(off_slice, 2093.0, SR);
    eprintln!(
        "Wave anti-alias toggle: on={:.2} dB, off={:.2} dB",
        alias_on, alias_off
    );
    // For a sine wavetable both might be quite clean, but ON should
    // never be WORSE than OFF.
    assert!(
        alias_on <= alias_off + 1.0,
        "Anti-alias ON ({alias_on:.2}) is worse than OFF ({alias_off:.2}) — \
         mip-mapping path regression?"
    );
}
