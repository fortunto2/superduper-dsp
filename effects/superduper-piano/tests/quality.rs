//! Quality snapshot for SuperDuper Piano, on a synthetic fixture pack so the
//! numbers never depend on what is installed on the machine running them.
//!
//!     SDSP_UPDATE_SNAPSHOTS=1 cargo test -p superduper-piano --test quality

use clack_host::events::event_types::{MidiEvent, NoteOffEvent, NoteOnEvent};
use clack_host::events::Pckn;
use sdsp_test_kit::probes::{self, db};
use sdsp_test_kit::host::render_with;
use sdsp_test_kit::{PluginUnderTest, Suite};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;

struct Target;
impl PluginUnderTest for Target {
    type Plugin = superduper_piano::SuperDuperPiano;
    const ID: &'static std::ffi::CStr = c"co.superduperai.piano";
}

const SR: f64 = 48_000.0;

/// A three-zone sine "piano": keycenters 48 / 60 / 72, two velocity layers
/// (soft = −12 dB), and a release-trigger click. Written once per process.
fn fixture_root() -> &'static PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("sdsp-piano-fixture-{}", std::process::id()));
        let pack = root.join("Fixture").join("Samples");
        std::fs::create_dir_all(&pack).expect("mkdir");
        let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let write = |name: &str, hz: f64, amp: f64, secs: f64| {
            let mut w = hound::WavWriter::create(pack.join(name), spec).expect("wav");
            let n = (secs * 48_000.0) as usize;
            for i in 0..n {
                let decay = (-(i as f64) / 48_000.0 * 1.5).exp();
                let v = (amp * decay * (2.0 * std::f64::consts::PI * hz * i as f64 / 48_000.0).sin() * 32767.0) as i16;
                w.write_sample(v).expect("w");
                w.write_sample(v).expect("w");
            }
            w.finalize().expect("fin");
        };
        for (key, hz) in [(48, 130.81), (60, 261.63), (72, 523.25)] {
            write(&format!("k{key}_hard.wav"), hz, 0.5, 3.0);
            write(&format!("k{key}_soft.wav"), hz, 0.125, 3.0);
        }
        write("rel.wav", 2000.0, 0.05, 0.05);
        let sfz = "<control> default_path=Samples/\n\
            <group> lovel=1 hivel=63\n\
            <region> sample=k48_soft.wav lokey=21 hikey=54 pitch_keycenter=48\n\
            <region> sample=k60_soft.wav lokey=55 hikey=66 pitch_keycenter=60\n\
            <region> sample=k72_soft.wav lokey=67 hikey=108 pitch_keycenter=72\n\
            <group> lovel=64 hivel=127\n\
            <region> sample=k48_hard.wav lokey=21 hikey=54 pitch_keycenter=48\n\
            <region> sample=k60_hard.wav lokey=55 hikey=66 pitch_keycenter=60\n\
            <region> sample=k72_hard.wav lokey=67 hikey=108 pitch_keycenter=72\n\
            <group> trigger=release\n\
            <region> sample=rel.wav lokey=21 hikey=108\n";
        std::fs::write(root.join("Fixture").join("Fixture Piano.sfz"), sfz).expect("sfz");
        root
    })
}

/// Render a note after the loader has delivered an instrument. The event
/// callback runs on the audio thread before each block; at block 0 it waits
/// (bounded) for the load, so the process() that follows swaps it in.
/// The load counter is process-wide, so two renders running at once would
/// each count the other's load as their own. One render at a time.
static RENDER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn render_note(key: u8, vel: f64, hold_s: f64, total_s: f64, pedal: bool) -> (Vec<f32>, Vec<f32>) {
    let _serial = RENDER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("SUPERDUPER_PIANO_ROOT", fixture_root());
    let before = superduper_piano::LOADS_COMPLETED.load(Ordering::SeqCst);
    let frames = (SR * total_s) as usize;
    let silence = vec![0.0_f32; frames];
    let off_at = (SR * hold_s) as usize;
    let block = 512;
    render_with::<Target>(SR, block, &silence, &silence, move |pos, buf| {
        if pos == 0 {
            let t0 = std::time::Instant::now();
            while superduper_piano::LOADS_COMPLETED.load(Ordering::SeqCst) == before
                && t0.elapsed().as_secs() < 20
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        // Note-on at block 1: block 0's process() swaps the instrument in.
        if pos == block {
            if pedal {
                buf.push(&MidiEvent::new(0, 0, [0xB0, 64, 127]));
            }
            buf.push(&NoteOnEvent::new(0, Pckn::new(0u16, 0u16, key as u16, 0u32), vel));
        }
        if pos <= off_at && off_at < pos + block {
            buf.push(&NoteOffEvent::new((off_at - pos) as u32, Pckn::new(0u16, 0u16, key as u16, 0u32), 0.0));
        }
    })
}

#[test]
fn every_note_sounds() {
    for key in (21..=108).step_by(7) {
        let (l, _) = render_note(key, 0.8, 0.3, 0.5, false);
        assert!(db(probes::rms(&l)) > -50.0, "key {key} is silent: {:.1} dB", db(probes::rms(&l)));
    }
}

#[test]
fn velocity_layers_and_pitch() {
    let (hard, _) = render_note(60, 1.0, 0.5, 0.6, false);
    let (soft, _) = render_note(60, 0.3, 0.5, 0.6, false);
    let diff = db(probes::rms(&hard)) - db(probes::rms(&soft));
    assert!(diff > 12.0, "soft layer (−12 dB sample) + velocity curve should be well below hard: {diff:.1} dB");
    // Key 62 plays the C4 zone two semitones up: 261.63 · 2^(2/12) ≈ 293.7 Hz.
    let (d4, _) = render_note(62, 1.0, 0.5, 0.6, false);
    let zc = d4[4800..24000].windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f64;
    let hz = zc / (19200.0 / SR);
    assert!((hz - 293.7).abs() < 3.0, "pitch {hz:.1} Hz");
}

#[test]
fn sustain_pedal_keeps_the_note_after_key_up() {
    let tail = |pedal: bool| {
        let (l, _) = render_note(60, 1.0, 0.2, 1.2, pedal);
        db(probes::rms(&l[(0.8 * SR) as usize..]))
    };
    let (held, dry) = (tail(true), tail(false));
    assert!(held > dry + 20.0, "pedal tail {held:.1} dB vs released {dry:.1} dB");
}

#[test]
fn quality_snapshot() {
    let mut s = Suite::new("piano", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/quality.snap")).default_tolerance(1.5);
    for key in [36, 48, 60, 72, 96] {
        let (l, _) = render_note(key, 0.8, 0.5, 1.5, false);
        s.record(format!("note{key}_rms_db"), db(probes::rms(&l)));
        s.record(format!("note{key}_peak_db"), db(probes::peak(&l)));
        s.record(format!("note{key}_max_step"), probes::max_step(&l));
    }
    sdsp_test_kit::params::record_table(&mut s, superduper_piano::PARAMS);
    s.finish();
}

#[test]
fn parameter_table_is_consistent() {
    sdsp_test_kit::params::check_table("superduper-piano", superduper_piano::PARAMS, &[]);
}

#[global_allocator]
static ALLOC: sdsp_test_kit::alloc::CountingAllocator = sdsp_test_kit::alloc::CountingAllocator;

#[test]
fn process_does_not_allocate() {
    sdsp_test_kit::alloc::reset();
    let _ = render_note(60, 0.8, 0.3, 0.8, true);
    sdsp_test_kit::alloc::assert_rt_clean("superduper-piano", 75);
}
