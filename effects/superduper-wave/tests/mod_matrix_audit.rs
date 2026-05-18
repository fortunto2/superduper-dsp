//! mod_matrix_audit.rs — drive Wave through CLAP twice (baseline vs.
//! ModWheel→Cutoff @ Amt=1.0 with CC#1=127), record both to /tmp/*.wav,
//! and assert the active phase has a meaningfully higher spectral
//! centroid than baseline.
//!
//! This is the *real* end-to-end audit for the mod matrix — it spins
//! up the plugin via clack-host, sends real NoteOn + MIDI CC + parameter
//! events through `process()`, and listens to the result.
//!
//! Run with:
//!   cargo test --release -p superduper-wave --test mod_matrix_audit -- --nocapture
//!
//! Audition:
//!   afplay /tmp/wave_modmatrix_baseline.wav
//!   afplay /tmp/wave_modmatrix_active.wav

use clack_common::events::Pckn;
use clack_common::events::event_types::{MidiEvent, ParamValueEvent};
use clack_common::utils::{ClapId, Cookie};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::event_types::NoteOnEvent;
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, AudioPorts};
use clack_plugin::entry::SinglePluginEntry;
use std::io::Write;
use superduper_synth_core::analysis::magnitude_spectrum_db;
use superduper_wave::SuperDuperWave;

const SR: f32 = 48_000.0;
const BLOCK: u32 = 256;
const TOTAL_SECONDS: f32 = 1.5;

// Wave param IDs we need to drive — must match the constants in lib.rs.
const P_CUTOFF: u32 = 4;
const P_NOISE: u32 = 14;
const P_MOD1_SRC: u32 = 27;
const P_MOD1_DST: u32 = 28;
const P_MOD1_AMT: u32 = 29;

struct TestHostShared;
impl SharedHandler<'_> for TestHostShared {
    fn request_restart(&self) {}
    fn request_process(&self) {}
    fn request_callback(&self) {}
}
impl HostLogImpl for TestHostShared {
    fn log(&self, severity: LogSeverity, message: &str) {
        eprintln!("[plugin {severity}] {message}");
    }
}
struct TestHost;
impl HostHandlers for TestHost {
    type Shared<'a> = TestHostShared;
    type MainThread<'a> = ();
    type AudioProcessor<'a> = ();
    fn declare_extensions(builder: &mut HostExtensions<Self>, _: &Self::Shared<'_>) {
        builder.register::<HostLog>();
    }
}

fn write_wav_i16_stereo(path: &str, sr: u32, l: &[f32], r: &[f32]) -> std::io::Result<()> {
    assert_eq!(l.len(), r.len());
    let n = l.len();
    let bytes_per_sample = 2u16;
    let channels = 2u16;
    let data_size = (n as u32) * (channels as u32) * (bytes_per_sample as u32);
    let file_size = 36 + data_size;
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&file_size.to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&sr.to_le_bytes())?;
    f.write_all(&(sr * channels as u32 * bytes_per_sample as u32).to_le_bytes())?;
    f.write_all(&(channels * bytes_per_sample).to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_size.to_le_bytes())?;
    for i in 0..n {
        let l_i = (l[i].clamp(-1.0, 1.0) * 32767.0) as i16;
        let r_i = (r[i].clamp(-1.0, 1.0) * 32767.0) as i16;
        f.write_all(&l_i.to_le_bytes())?;
        f.write_all(&r_i.to_le_bytes())?;
    }
    Ok(())
}

/// Spectral centroid — sum(freq * magnitude) / sum(magnitude). The
/// brightness measure that scales with where the energy sits in the
/// frequency axis. A low-pass filter at 4 kHz vs. ~23 kHz should
/// produce dramatically different centroids on the same source.
fn spectral_centroid(samples: &[f32], sr: f32) -> f32 {
    let spec_db = magnitude_spectrum_db(samples);
    // Convert dB back to linear magnitude for the moment calculation —
    // dB-weighted centroids are common but the linear version maps more
    // intuitively to where the energy actually is.
    let n_bins = spec_db.len();
    let mut weighted = 0.0_f32;
    let mut total = 0.0_f32;
    for (i, &db) in spec_db.iter().enumerate() {
        // Ignore DC + Nyquist bin.
        if i == 0 || i == n_bins - 1 {
            continue;
        }
        let mag = 10.0_f32.powf(db * 0.05);
        let freq = (i as f32) * sr / ((n_bins - 1) as f32 * 2.0);
        weighted += freq * mag;
        total += mag;
    }
    if total <= 0.0 { 0.0 } else { weighted / total }
}

/// Render `total_seconds` of audio through Wave with one block-0 setup:
/// the supplied initial parameter+MIDI events fire on the very first
/// process block (sample 0). NoteOn key=60 vel=1.0 also fires at block 0.
fn render(
    label: &str,
    initial_events: impl Fn(u32) -> Vec<EventEnum> + Send + 'static,
) -> (Vec<f32>, Vec<f32>) {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperWave>>(
        c"/in/process/test/superduper-wave-modmatrix",
    )
    .expect("plugin entry should load");

    let host_info = HostInfo::new("SDSP Test", "SuperDuperAI", "https://superduperai.co", "0")
        .unwrap();
    let mut plugin = PluginInstance::<TestHost>::new(
        |_| TestHostShared,
        |_| (),
        &entry,
        c"co.superduperai.wave",
        &host_info,
    )
    .expect("plugin should instantiate");

    const BLOCK_USIZE: usize = BLOCK as usize;
    let total_frames: usize = (SR as usize) * TOTAL_SECONDS as usize;
    let n_blocks = total_frames / BLOCK_USIZE;
    let cfg = PluginAudioConfiguration {
        sample_rate: SR as f64,
        min_frames_count: BLOCK,
        max_frames_count: BLOCK,
    };
    let stopped = plugin.activate(|_, _| (), cfg).expect("activate");

    let mut all_l = vec![0.0_f32; total_frames];
    let mut all_r = vec![0.0_f32; total_frames];
    let l_ref = &mut all_l;
    let r_ref = &mut all_r;

    let stopped_back = std::thread::scope(|s| {
        s.spawn(move || {
            let mut audio_proc = stopped.start_processing().expect("start_processing");
            let mut input_ports = AudioPorts::with_capacity(0, 0);
            let mut output_ports = AudioPorts::with_capacity(2, 1);

            for block in 0..n_blocks {
                let start = block * BLOCK_USIZE;
                let end = start + BLOCK_USIZE;

                let mut input_buf = EventBuffer::new();
                if block == 0 {
                    for ev in initial_events(0) {
                        match ev {
                            EventEnum::Param(e) => input_buf.push(&e),
                            EventEnum::Midi(e) => input_buf.push(&e),
                            EventEnum::Note(e) => input_buf.push(&e),
                        }
                    }
                }
                let input_events = InputEvents::from_buffer(&input_buf);
                let mut out_evs = EventBuffer::new();
                let mut output_events = OutputEvents::from_buffer(&mut out_evs);

                let mut out_l_chunk = vec![0.0_f32; BLOCK_USIZE];
                let mut out_r_chunk = vec![0.0_f32; BLOCK_USIZE];

                let input_audio = input_ports.with_input_buffers(std::iter::empty::<
                    AudioPortBuffer<
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f32>>,
                        std::iter::Empty<clack_host::process::audio_buffers::InputChannel<f64>>,
                    >,
                >());

                let mut out_chans: [&mut [f32]; 2] =
                    [out_l_chunk.as_mut_slice(), out_r_chunk.as_mut_slice()];
                let mut output_audio = output_ports.with_output_buffers([AudioPortBuffer {
                    latency: 0,
                    channels: AudioPortBufferType::f32_output_only(
                        out_chans.iter_mut().map(|b| &mut **b),
                    ),
                }]);

                audio_proc
                    .process(
                        &input_audio,
                        &mut output_audio,
                        &input_events,
                        &mut output_events,
                        None,
                        None,
                    )
                    .expect("process should succeed");

                l_ref[start..end].copy_from_slice(&out_l_chunk);
                r_ref[start..end].copy_from_slice(&out_r_chunk);
            }
            audio_proc.stop_processing()
        })
        .join()
        .expect("audio thread")
    });
    plugin.deactivate(stopped_back);

    let _ = label;
    (all_l, all_r)
}

/// Sum-type so we can stuff different event flavours into one Vec.
enum EventEnum {
    Param(ParamValueEvent),
    Midi(MidiEvent),
    Note(NoteOnEvent),
}

#[test]
fn wave_mod_matrix_modwheel_to_cutoff_lifts_centroid() {
    // ---- Phase A — baseline (mod matrix off, default cutoff 4 kHz) ----
    let (a_l, a_r) = render("baseline", |t| {
        // Force cutoff to a clear baseline (1.2 kHz) — so the active
        // case has plenty of headroom to lift it.  Drive is pushed to
        // saturate the default sine wavetable into a harmonic-rich
        // signal — otherwise a pure-tone fundamental at 261 Hz passes
        // through both filter settings unchanged and the centroid is
        // identical (the bug we hit on the first attempt).
        let cutoff = ParamValueEvent::new(
            t, ClapId::new(P_CUTOFF), Pckn::new(0u16, 0u16, 0u16, 0u32),
            1200.0, Cookie::empty(),
        );
        // Crank the per-voice noise so we have a broadband signal the
        // lowpass can actually shape — the default sine wavetable is
        // harmonic-poor, so the centroid wouldn't move when we sweep
        // the cutoff.
        let noise = ParamValueEvent::new(
            t, ClapId::new(P_NOISE), Pckn::new(0u16, 0u16, 0u16, 0u32),
            1.0, Cookie::empty(),
        );
        let note = NoteOnEvent::new(t, Pckn::new(0u16, 0u16, 60u16, 0u32), 1.0);
        vec![EventEnum::Param(cutoff), EventEnum::Param(noise), EventEnum::Note(note)]
    });
    let baseline_centroid = {
        // Use the steady-state second half of the buffer (skip attack).
        let half = a_l.len() / 2;
        let mono: Vec<f32> = a_l[half..]
            .iter()
            .zip(a_r[half..].iter())
            .map(|(l, r)| 0.5 * (l + r))
            .collect();
        spectral_centroid(&mono, SR)
    };
    write_wav_i16_stereo("/tmp/wave_modmatrix_baseline.wav", SR as u32, &a_l, &a_r)
        .expect("baseline wav");

    // ---- Phase B — mod matrix slot 1: ModWheel → Cutoff @ Amt=1.0,
    //                                   CC#1 = 127 (full wheel up) ----
    let (b_l, b_r) = render("active", |t| {
        let cutoff = ParamValueEvent::new(
            t,
            ClapId::new(P_CUTOFF),
            Pckn::new(0u16, 0u16, 0u16, 0u32),
            1200.0,
            Cookie::empty(),
        );
        // Crank the per-voice noise so we have a broadband signal the
        // lowpass can actually shape — the default sine wavetable is
        // harmonic-poor, so the centroid wouldn't move when we sweep
        // the cutoff.
        let noise = ParamValueEvent::new(
            t, ClapId::new(P_NOISE), Pckn::new(0u16, 0u16, 0u16, 0u32),
            1.0, Cookie::empty(),
        );
        let src = ParamValueEvent::new(
            t,
            ClapId::new(P_MOD1_SRC),
            Pckn::new(0u16, 0u16, 0u16, 0u32),
            3.0, // ModWheel
            Cookie::empty(),
        );
        let dst = ParamValueEvent::new(
            t,
            ClapId::new(P_MOD1_DST),
            Pckn::new(0u16, 0u16, 0u16, 0u32),
            1.0, // Cutoff
            Cookie::empty(),
        );
        let amt = ParamValueEvent::new(
            t,
            ClapId::new(P_MOD1_AMT),
            Pckn::new(0u16, 0u16, 0u16, 0u32),
            1.0,
            Cookie::empty(),
        );
        // CC#1 = 127 — wheel pushed to the top.
        let cc = MidiEvent::new(t, 0, [0xB0, 1, 127]);
        let note = NoteOnEvent::new(t, Pckn::new(0u16, 0u16, 60u16, 0u32), 1.0);
        vec![
            EventEnum::Param(cutoff),
            EventEnum::Param(noise),
            EventEnum::Param(src),
            EventEnum::Param(dst),
            EventEnum::Param(amt),
            EventEnum::Midi(cc),
            EventEnum::Note(note),
        ]
    });
    let active_centroid = {
        let half = b_l.len() / 2;
        let mono: Vec<f32> = b_l[half..]
            .iter()
            .zip(b_r[half..].iter())
            .map(|(l, r)| 0.5 * (l + r))
            .collect();
        spectral_centroid(&mono, SR)
    };
    write_wav_i16_stereo("/tmp/wave_modmatrix_active.wav", SR as u32, &b_l, &b_r)
        .expect("active wav");

    // ---- Stats + audition hint ----
    eprintln!("Wrote /tmp/wave_modmatrix_baseline.wav + /tmp/wave_modmatrix_active.wav");
    eprintln!(
        "baseline centroid = {:.0} Hz, active centroid = {:.0} Hz (ratio {:.2}x)",
        baseline_centroid,
        active_centroid,
        active_centroid / baseline_centroid.max(1.0)
    );

    // ---- Assertion ----
    // With cutoff at 1.2 kHz and Amt=1.0 (4 octaves up at full wheel),
    // the active phase should be running near Nyquist. We expect the
    // centroid to roughly double or more. If it's not at least 1.5x
    // higher, something is wrong with the matrix routing.
    assert!(
        active_centroid > baseline_centroid * 1.5,
        "Mod matrix didn't audibly lift centroid: baseline={:.0} Hz, active={:.0} Hz \
         (expected active > 1.5 * baseline). Audit the two WAVs in /tmp/.",
        baseline_centroid,
        active_centroid
    );
    // Also assert: the active phase actually produces non-trivial audio
    // (catch the regression where we accidentally route Volume to 0).
    let active_rms: f32 =
        (b_l.iter().map(|x| x * x).sum::<f32>() / b_l.len() as f32).sqrt();
    assert!(
        active_rms > 0.005,
        "Active phase output is suspiciously silent (RMS={:.4}). Routing broken?",
        active_rms
    );
}
