//! click_audit.rs — drive the Pad through a realistic MIDI sequence,
//! record the output to /tmp/pad_click_audit.wav, and assert against
//! sample-to-sample discontinuities. Always prints a discontinuity
//! histogram and an ASCII spectrum of the release tail so the WAV can be
//! audited even when the test passes.
//!
//! Run with:
//!   cargo test --release -p superduper-pad --test click_audit -- --nocapture
//!
//! Then audition the WAV:
//!   afplay /tmp/pad_click_audit.wav   (macOS)

use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_host::events::Pckn;
use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_host::events::io::{EventBuffer, InputEvents, OutputEvents};
use clack_host::prelude::*;
use clack_host::process::audio_buffers::{AudioPortBuffer, AudioPortBufferType, AudioPorts};
use clack_plugin::entry::SinglePluginEntry;
use std::io::Write;
use superduper_pad::SuperDuperPad;
use superduper_synth_core::analysis::{AsciiSpectrumOpts, ascii_spectrum, spectrum_with_freq};

// ---------------------------------------------------------------------------
// CLAP host plumbing — minimal, copied from clap_midi.rs.
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Test config + MIDI sequence.
// ---------------------------------------------------------------------------

const SR: f32 = 48_000.0;
const BLOCK: u32 = 256; // small blocks → more event-seam stress
const TOTAL_SECONDS: f32 = 6.0;

#[derive(Clone, Copy, Debug)]
enum MidiAt {
    On(u8, f32),
    Off(u8),
}

fn build_sequence() -> Vec<(usize, MidiAt)> {
    // Three phases: held chord (4 voices), staggered release, then voice-steal
    // stress (12 staccato notes — exceeds the 8-voice pool).
    let t = |s: f32| (s * SR) as usize;
    let mut seq = vec![
        // Phase 1 — build a C-major chord one note at a time.
        (t(0.10), MidiAt::On(60, 0.90)),
        (t(0.35), MidiAt::On(64, 0.85)),
        (t(0.60), MidiAt::On(67, 0.80)),
        (t(0.85), MidiAt::On(72, 0.75)),
        // Phase 2 — staggered release; tests overlap of new attack while
        // older notes are still releasing.
        (t(1.50), MidiAt::Off(60)),
        (t(1.75), MidiAt::Off(64)),
        (t(2.00), MidiAt::Off(67)),
        (t(2.25), MidiAt::Off(72)),
    ];
    // Phase 3 — voice steal stress.
    let stab_start = 2.7;
    let stab_keys: [u8; 12] = [48, 50, 52, 53, 55, 57, 59, 60, 62, 64, 65, 67];
    for (i, k) in stab_keys.iter().enumerate() {
        seq.push((t(stab_start + i as f32 * 0.05), MidiAt::On(*k, 0.85)));
    }
    // All-notes-off at the end.
    let off_t = stab_start + stab_keys.len() as f32 * 0.05 + 0.2;
    for k in stab_keys.iter() {
        seq.push((t(off_t), MidiAt::Off(*k)));
    }
    seq.sort_by_key(|(p, _)| *p);
    seq
}

// ---------------------------------------------------------------------------
// Minimal 16-bit PCM WAV writer — avoids pulling `hound` as a dev-dep just
// for one test. Inline because we never need it elsewhere.
// ---------------------------------------------------------------------------

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
    f.write_all(&16u32.to_le_bytes())?; // PCM chunk size
    f.write_all(&1u16.to_le_bytes())?; // format = PCM
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&sr.to_le_bytes())?;
    f.write_all(&(sr * channels as u32 * bytes_per_sample as u32).to_le_bytes())?;
    f.write_all(&(channels * bytes_per_sample).to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_size.to_le_bytes())?;
    // Interleaved L/R.
    for i in 0..n {
        let l_i = (l[i].clamp(-1.0, 1.0) * 32767.0) as i16;
        let r_i = (r[i].clamp(-1.0, 1.0) * 32767.0) as i16;
        f.write_all(&l_i.to_le_bytes())?;
        f.write_all(&r_i.to_le_bytes())?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Discontinuity analysis.
//
// "Click" heuristic: a single-sample jump |x[n]-x[n-1]| that's larger than
// the local short-term peak amplitude would predict.  For a bandlimited
// pad signal at moderate volume, the per-sample slew is bounded by the
// highest frequency × period × peak; an abrupt step (filter reset, voice
// hard-cut) shows up as a sample-diff outlier.
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct DiscontinuityStats {
    max_jump: f32,
    max_jump_at_sample: usize,
    suspects_above_threshold: usize,
    histogram: [u32; 11],
}

fn analyse_channel(signal: &[f32], threshold: f32) -> DiscontinuityStats {
    let mut max_jump = 0.0_f32;
    let mut max_jump_at = 0usize;
    let mut suspects = 0usize;
    let mut histogram = [0u32; 11];
    for i in 1..signal.len() {
        let d = (signal[i] - signal[i - 1]).abs();
        if d > max_jump {
            max_jump = d;
            max_jump_at = i;
        }
        if d > threshold {
            suspects += 1;
        }
        let bin = ((d * 10.0) as usize).min(10);
        histogram[bin] += 1;
    }
    DiscontinuityStats {
        max_jump,
        max_jump_at_sample: max_jump_at,
        suspects_above_threshold: suspects,
        histogram,
    }
}

// ---------------------------------------------------------------------------
// The test itself.
// ---------------------------------------------------------------------------

#[test]
fn pad_click_audit() {
    let entry = PluginEntry::load_from_clack::<SinglePluginEntry<SuperDuperPad>>(
        c"/in/process/test/superduper-pad-click-audit",
    )
    .expect("plugin entry should load");

    let host_info = HostInfo::new("SDSP Test", "SuperDuperAI", "https://superduperai.co", "0")
        .unwrap();
    let mut plugin = PluginInstance::<TestHost>::new(
        |_| TestHostShared,
        |_| (),
        &entry,
        c"co.superduperai.pad",
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

    let sequence = build_sequence();

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

                // Collect MIDI events that fall into [start, end).
                let mut input_buf = EventBuffer::new();
                for (pos, ev) in &sequence {
                    if *pos >= start && *pos < end {
                        let local = (*pos - start) as u32;
                        match *ev {
                            MidiAt::On(key, vel) => {
                                let pckn = Pckn::new(0u16, 0u16, key as u16, 0u32);
                                let e = NoteOnEvent::new(local, pckn, vel as f64);
                                input_buf.push(&e);
                            }
                            MidiAt::Off(key) => {
                                let pckn = Pckn::new(0u16, 0u16, key as u16, 0u32);
                                let e = NoteOffEvent::new(local, pckn, 1.0);
                                input_buf.push(&e);
                            }
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

    // Save WAV for human listening.
    let wav_path = "/tmp/pad_click_audit.wav";
    write_wav_i16_stereo(wav_path, SR as u32, &all_l, &all_r).expect("WAV write");
    eprintln!("Wrote {wav_path} — `afplay {wav_path}` (macOS) to audition.");

    // Per-channel stats.
    let threshold = 0.15_f32;
    let l_stats = analyse_channel(&all_l, threshold);
    let r_stats = analyse_channel(&all_r, threshold);
    eprintln!(
        "[L] max |Δx| = {:.4} at sample {} (t={:.3}s); jumps > {threshold:.2}: {}",
        l_stats.max_jump,
        l_stats.max_jump_at_sample,
        l_stats.max_jump_at_sample as f32 / SR,
        l_stats.suspects_above_threshold,
    );
    eprintln!(
        "[R] max |Δx| = {:.4} at sample {} (t={:.3}s); jumps > {threshold:.2}: {}",
        r_stats.max_jump,
        r_stats.max_jump_at_sample,
        r_stats.max_jump_at_sample as f32 / SR,
        r_stats.suspects_above_threshold,
    );
    eprintln!(
        "[L] |Δ| histogram (bins 0..0.1, 0.1..0.2, …, ≥1.0): {:?}",
        l_stats.histogram
    );

    // Print the top 10 worst jumps and their time so we know where to look.
    let mut indexed: Vec<(usize, f32)> = (1..all_l.len())
        .map(|i| (i, (all_l[i] - all_l[i - 1]).abs().max((all_r[i] - all_r[i - 1]).abs())))
        .collect();
    indexed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    eprintln!("Top 10 |Δ| spikes (with timing):");
    for &(i, d) in indexed.iter().take(10) {
        eprintln!("  sample {i:6} t={:.4}s  Δ={:.4}", i as f32 / SR, d);
    }

    // ASCII spectrum of the release tail (last 0.7 s — well past all NoteOff).
    let tail_n = (SR as usize) * 7 / 10;
    let tail_start = all_l.len().saturating_sub(tail_n);
    let tail_mono: Vec<f32> = (tail_start..all_l.len())
        .map(|i| 0.5 * (all_l[i] + all_r[i]))
        .collect();
    let fft_len = 8192.min(tail_mono.len().next_power_of_two() / 2);
    if fft_len >= 4096 {
        let slice = &tail_mono[tail_mono.len() - fft_len..];
        let spec = spectrum_with_freq(slice, SR);
        let opts = AsciiSpectrumOpts {
            rows: 14,
            cols: 100,
            min_db: -100.0,
            max_db: -10.0,
            ..Default::default()
        };
        eprintln!("\nRelease-tail spectrum (last {fft_len} samples):");
        eprint!("{}", ascii_spectrum(&spec, &opts));
    }

    // Hard assertion — at 48 kHz, a clean pad voice with our highest
    // partial near a few kHz should never produce a sample-to-sample
    // jump above ~0.4. Anything past that is an audible click.
    assert!(
        l_stats.max_jump < 0.4 && r_stats.max_jump < 0.4,
        "Audible click suspected — max |Δx|: L={:.3} R={:.3} (threshold 0.4). \
         Audit /tmp/pad_click_audit.wav for confirmation.",
        l_stats.max_jump,
        r_stats.max_jump
    );
}
