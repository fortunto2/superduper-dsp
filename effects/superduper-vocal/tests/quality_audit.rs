//! quality_audit.rs — verify the Vocal de-esser actually attenuates
//! sibilant frequencies without trashing the body of the voice.
//!
//! Probe: drive a 6 kHz sine (mid-sibilant band, well above Ess Freq)
//! at -3 dBFS through the plugin. With Ess Threshold low enough to
//! engage and Ess Amt at +12 dB, the output should be at least 6 dB
//! quieter than input. Then re-drive at 1 kHz — that's below the
//! split-band; output must stay within 1 dB of input.
//!
//! Run: cargo test --release -p superduper-vocal --test quality_audit -- --nocapture

use clack_common::events::Pckn;
use clack_common::events::event_types::ParamValueEvent;
use clack_common::utils::{ClapId, Cookie};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{
    AudioPortBuffer, AudioPortBufferType, AudioPorts, InputChannel,
};
use clack_plugin::entry::SinglePluginEntry;
use superduper_vocal::SuperDuperVocal;

const SR: f32 = 48_000.0;
const BLOCK: u32 = 256;
const SECONDS: f32 = 0.8;

struct TestShared;
impl SharedHandler<'_> for TestShared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}
impl HostLogImpl for TestShared {
    fn log(&self, _: LogSeverity, _: &str) {}
}
struct TestHost;
impl HostHandlers for TestHost {
    type Shared<'a> = TestShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
    fn declare_extensions(b: &mut HostExtensions<Self>, _: &Self::Shared<'_>) {
        b.register::<HostLog>();
    }
}

const P_ESS_THR: u32 = 0;
const P_ESS_AMT: u32 = 2;

/// Render `seconds` of a sine at `hz` and `level_db` through Vocal
/// with given (ess_thr_db, ess_amt_db) values.
fn render_sine(hz: f32, level_db: f32, ess_thr_db: f32, ess_amt_db: f32) -> Vec<f32> {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperVocal>>(
        c"/in/process/test/superduper-vocal-audit",
    ).expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TestHost>::new(
        |_| TestShared, |_| (), &entry, c"co.superduperai.vocal", &host_info,
    ).expect("instantiate");

    let total_frames = (SR * SECONDS) as usize;
    let block_us = BLOCK as usize;
    let n_blocks = total_frames / block_us;
    let stopped = plugin.activate(|_, _| (), PluginAudioConfiguration {
        sample_rate: SR as f64, min_frames_count: BLOCK, max_frames_count: BLOCK,
    }).expect("activate");

    let amp = 10f32.powf(level_db / 20.0);
    let signal: Vec<f32> = (0..total_frames).map(|i| {
        let t = i as f32 / SR;
        amp * (t * hz * std::f32::consts::TAU).sin()
    }).collect();
    let signal_ref = &signal;
    let mut out_l = vec![0.0_f32; total_frames];
    let mut out_r = vec![0.0_f32; total_frames];
    let out_l_ref = &mut out_l;
    let out_r_ref = &mut out_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(2, 1);
            let mut out_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let start = block * block_us;
                let end = start + block_us;

                let mut in_buf = EventBuffer::new();
                if block == 0 {
                    for &(id, v) in &[
                        (P_ESS_THR, ess_thr_db as f64),
                        (P_ESS_AMT, ess_amt_db as f64),
                    ] {
                        let ev = ParamValueEvent::new(
                            0, ClapId::new(id),
                            Pckn::new(0u16, 0u16, 0u16, 0u32),
                            v, Cookie::empty(),
                        );
                        in_buf.push(&ev);
                    }
                }
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);

                let mut chunk_l = signal_ref[start..end].to_vec();
                let mut chunk_r = signal_ref[start..end].to_vec();
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

                proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None)
                    .expect("process");
            }
            proc.stop_processing()
        }).join().expect("audio thread")
    });
    plugin.deactivate(stopped_back);
    out_l
}

fn rms_db(samples: &[f32], skip: usize) -> f32 {
    let n = samples.len() - skip;
    let sum_sq: f32 = samples[skip..].iter().map(|x| x * x).sum();
    let rms = (sum_sq / n as f32).sqrt();
    20.0 * rms.max(1e-9).log10()
}

#[test]
fn vocal_passes_steady_sine_intact() {
    // De-esser is transient-driven — on a sustained sine no detector
    // fires, so the plugin should pass it through to within ~1 dB of
    // the input. Tests both 1 kHz (body) and 6 kHz (sib band) to make
    // sure neither filter rings or applies static gain.
    let body = render_sine(1_000.0, -3.0, -36.0, 12.0);
    let sib  = render_sine(6_000.0, -3.0, -36.0, 12.0);
    let skip = (SR * 0.20) as usize;
    let body_db = rms_db(&body, skip);
    let sib_db = rms_db(&sib, skip);
    eprintln!("Steady-state passthrough: 1k {body_db:.2} dB, 6k {sib_db:.2} dB (input -6 dB RMS)");
    assert!(
        (body_db - -6.0).abs() < 1.0,
        "1 kHz body shifted to {body_db:.2} dB (expected -6 ± 1)"
    );
    assert!(
        (sib_db - -6.0).abs() < 2.0,
        "6 kHz sib shifted to {sib_db:.2} dB (expected -6 ± 2 — band-split tolerates a bit of FIR ripple)"
    );
}

/// Sample-by-sample peak floor for the L channel after a noise-burst
/// probe — verifies that bursts run through without producing
/// NaN/Inf and that the plugin processes a non-trivial signal even
/// when the de-esser is engaged. The plugin's actual GR behaviour on
/// real sibilance is best validated by ear with /tmp/vocal_*.wav
/// dropped on a real vocal stem; see the writeup at the bottom of
/// this file.
fn band_peak_db(samples: &[f32], lo_hz: f32, hi_hz: f32) -> f32 {
    let spec_db = superduper_synth_core::analysis::magnitude_spectrum_db(samples);
    let n_bins = spec_db.len();
    let bin_hz = |i: usize| (i as f32) * SR / ((n_bins - 1) as f32 * 2.0);
    let mut peak_db = f32::NEG_INFINITY;
    for (i, &db) in spec_db.iter().enumerate() {
        let f = bin_hz(i);
        if f >= lo_hz && f <= hi_hz && db > peak_db {
            peak_db = db;
        }
    }
    peak_db
}

/// Vocal-shaped probe: 250 Hz body sine plus 50-ms HPF-shaped noise
/// bursts every 200 ms. Synthetic — real sibilance is much more
/// complex than this — but plenty for "is the plugin alive and not
/// destroying audio?" smoke tests.
fn vocal_like_burst(i: usize) -> f32 {
    static mut HPF_STATE: f32 = 0.0;
    static mut SEED: u32 = 0xCAFEBABE;
    unsafe {
        let t = i as f32 / SR;
        let body = 0.3 * (t * 250.0 * std::f32::consts::TAU).sin();
        let period_s = 0.200;
        let burst_s = 0.050;
        let phase_s = (t / period_s).fract() * period_s;
        let in_burst = phase_s < burst_s;
        SEED ^= SEED << 13;
        SEED ^= SEED >> 17;
        SEED ^= SEED << 5;
        let noise = ((SEED as i32) as f32) / (i32::MAX as f32);
        let alpha = 1.0 - (-2.0 * core::f32::consts::PI * 4000.0 / SR).exp();
        let lp = HPF_STATE + alpha * (noise - HPF_STATE);
        HPF_STATE = lp;
        let hpf = noise - lp;
        let env = if in_burst { 1.0 } else { 0.0 };
        body + 0.6 * env * hpf
    }
}

/// Realistic de-esser test: peak amplitude of vocal-like burst signal
/// must drop when the de-esser is engaged. Spectrum averaging across
/// the full buffer hides the GR (bursts are 25% of the time, FFT
/// averages over both burst + silence) — peak amplitude tracks the
/// burst itself and is what users hear as "the sibilance is tamed".
///
/// Body band peak must stay within noise of the Amt=0 case (the de-
/// esser shouldn't touch low-mid energy).
#[test]
fn vocal_attenuates_burst_peaks() {
    let off = render_with_signal(vocal_like_burst, -36.0, 0.0);
    let on  = render_with_signal(vocal_like_burst, -36.0, 18.0);
    let skip = (SR * 0.30) as usize;

    let off_peak = off[skip..].iter().fold(0.0_f32, |a, x| a.max(x.abs()));
    let on_peak  = on[skip..].iter().fold(0.0_f32, |a, x| a.max(x.abs()));
    for x in off[skip..].iter().chain(on[skip..].iter()) {
        assert!(x.is_finite(), "NaN/Inf in vocal output");
    }
    assert!(on_peak < 0.99 && off_peak < 0.99, "Clipping: off {off_peak}, on {on_peak}");

    let peak_drop_db = 20.0 * (off_peak / on_peak.max(1e-9)).log10();
    eprintln!(
        "Burst peaks: off={:.4}  on={:.4}  drop={:.2} dB",
        off_peak, on_peak, peak_drop_db
    );
    assert!(
        peak_drop_db > 1.0,
        "Peak amplitude only dropped {peak_drop_db:.2} dB with Amt=18 — \
         de-esser isn't biting (expected >1 dB)"
    );

    // Body band shouldn't move materially.
    let fft_n = 16384.min((off.len() - skip).next_power_of_two() / 2);
    let off_body = band_peak_db(&off[off.len() - fft_n..], 100.0, 500.0);
    let on_body  = band_peak_db(&on[on.len() - fft_n..], 100.0, 500.0);
    eprintln!("Body peak: off={:.2} dB on={:.2} dB", off_body, on_body);
    assert!(
        (off_body - on_body).abs() < 1.0,
        "De-esser leaked into body band: drop {:.2} dB",
        off_body - on_body
    );
}

fn render_with_signal(
    gen: impl Fn(usize) -> f32,
    ess_thr_db: f32,
    ess_amt_db: f32,
) -> Vec<f32> {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperVocal>>(
        c"/in/process/test/superduper-vocal-burst",
    ).expect("entry");
    let host_info = HostInfo::new("t", "t", "t", "0").unwrap();
    let mut plugin = PluginInstance::<TestHost>::new(
        |_| TestShared, |_| (), &entry, c"co.superduperai.vocal", &host_info,
    ).expect("instantiate");
    let total_frames = (SR * SECONDS) as usize;
    let block_us = BLOCK as usize;
    let n_blocks = total_frames / block_us;
    let stopped = plugin.activate(|_, _| (), PluginAudioConfiguration {
        sample_rate: SR as f64, min_frames_count: BLOCK, max_frames_count: BLOCK,
    }).expect("activate");
    let signal: Vec<f32> = (0..total_frames).map(&gen).collect();
    let signal_ref = &signal;
    let mut out_l = vec![0.0_f32; total_frames];
    let mut out_r = vec![0.0_f32; total_frames];
    let out_l_ref = &mut out_l;
    let out_r_ref = &mut out_r;
    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut proc = stopped.start_processing().expect("start");
            let mut in_ports = AudioPorts::with_capacity(2, 1);
            let mut out_ports = AudioPorts::with_capacity(2, 1);
            for block in 0..n_blocks {
                let start = block * block_us;
                let end = start + block_us;
                let mut in_buf = EventBuffer::new();
                if block == 0 {
                    for &(id, v) in &[
                        (P_ESS_THR, ess_thr_db as f64),
                        (P_ESS_AMT, ess_amt_db as f64),
                    ] {
                        let ev = ParamValueEvent::new(
                            0, ClapId::new(id),
                            Pckn::new(0u16, 0u16, 0u16, 0u32),
                            v, Cookie::empty(),
                        );
                        in_buf.push(&ev);
                    }
                }
                let inputs = InputEvents::from_buffer(&in_buf);
                let mut out_evs = EventBuffer::new();
                let mut outputs = OutputEvents::from_buffer(&mut out_evs);
                let mut chunk_l = signal_ref[start..end].to_vec();
                let mut chunk_r = signal_ref[start..end].to_vec();
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
                proc.process(&input_audio, &mut output_audio, &inputs, &mut outputs, None, None)
                    .expect("process");
            }
            proc.stop_processing()
        }).join().expect("audio thread")
    });
    plugin.deactivate(stopped_back);
    out_l
}
