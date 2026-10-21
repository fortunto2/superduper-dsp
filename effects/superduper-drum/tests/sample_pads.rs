//! Sample pads on a fixture library: a pad switched to Sample plays the
//! browsed file, Layer adds the synth voice back, a closed hat chokes an open
//! hat sample, and none of it allocates on the audio thread.

use clack_host::events::event_types::{NoteOnEvent, ParamValueEvent};
use clack_host::events::Pckn;
use clack_host::utils::{ClapId, Cookie};
use sdsp_test_kit::host::render_with;
use sdsp_test_kit::probes::{self, db};
use sdsp_test_kit::PluginUnderTest;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::OnceLock;
use superduper_drum::{pads, P_SMP0, P_SRC0};

struct Target;
impl PluginUnderTest for Target {
    type Plugin = superduper_drum::SuperDuperDrum;
    const ID: &'static std::ffi::CStr = c"co.superduperai.drum";
}

const SR: f64 = 48_000.0;

fn fixture() -> &'static PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("sdsp-drum-fixture-{}", std::process::id()));
        let dir = root.join("Fixture Kit");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let tone = |name: &str, hz: f64, secs: f64| {
            let mut w = hound::WavWriter::create(dir.join(name), spec).expect("wav");
            for i in 0..(secs * 48_000.0) as usize {
                let t = i as f64 / 48_000.0;
                w.write_sample(((2.0 * std::f64::consts::PI * hz * t).sin() * 0.5 * 32767.0) as i16).expect("w");
            }
            w.finalize().expect("fin");
        };
        // Pitched "drums" so the test can tell which file played.
        tone("Kick_A.wav", 500.0, 1.0);
        tone("Kick_B.wav", 1000.0, 1.0);
        tone("Closed_Hat_01.wav", 3000.0, 0.5);
        tone("Open_Hat_01.wav", 2000.0, 2.0);
        root
    })
}

fn set(buf: &mut clack_host::events::io::EventBuffer, id: usize, v: f64) {
    buf.push(&ParamValueEvent::new(0, ClapId::new(id as u32), Pckn::match_all(), v, Cookie::empty()));
}

/// Set params at block 0, wait (bounded) for `loads` sample deliveries, then
/// play `notes` (key, block) and return the left channel.
/// The load counter is process-wide, so two renders running at once would
/// each count the other's load as their own. One render at a time.
static RENDER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn render(params: &[(usize, f64)], loads: u32, notes: &[(u8, usize)], secs: f64) -> Vec<f32> {
    let _serial = RENDER_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("SUPERDUPER_DRUM_ROOT", fixture());
    let before = pads::PAD_LOADS.load(Ordering::SeqCst);
    let frames = (SR * secs) as usize;
    let silence = vec![0.0f32; frames];
    let params = params.to_vec();
    let notes = notes.to_vec();
    let (l, _) = render_with::<Target>(SR, 512, &silence, &silence, move |pos, buf| {
        if pos == 0 { for &(id, v) in &params { set(buf, id, v); } }
        if pos == 512 {
            let t0 = std::time::Instant::now();
            while pads::PAD_LOADS.load(Ordering::SeqCst) < before + loads && t0.elapsed().as_secs() < 20 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        for &(key, block) in &notes {
            if pos == block * 512 { buf.push(&NoteOnEvent::new(0, Pckn::new(0u16, 0u16, key as u16, 0u32), 1.0)); }
        }
    });
    l
}

fn hz(x: &[f32]) -> f64 {
    let zc = x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count() as f64;
    zc / (x.len() as f64 / SR)
}

#[test]
fn sample_pad_plays_the_browsed_file() {
    let a = render(&[(P_SRC0, 1.0), (P_SMP0, 0.0)], 1, &[(36, 3)], 0.4);
    let b = render(&[(P_SRC0, 1.0), (P_SMP0, 1.0)], 1, &[(36, 3)], 0.4);
    let win = 3 * 512 + 480..3 * 512 + 4800;
    assert!((hz(&a[win.clone()]) - 500.0).abs() < 15.0, "Kick_A: {:.0} Hz", hz(&a[win.clone()]));
    assert!((hz(&b[win.clone()]) - 1000.0).abs() < 25.0, "Kick_B: {:.0} Hz", hz(&b[win]));
}

#[test]
fn layer_adds_the_synth_and_synth_ignores_the_sample() {
    let tail = 3 * 512..(SR * 0.3) as usize;
    let synth = render(&[(P_SRC0, 0.0)], 0, &[(36, 3)], 0.3);
    let sample = render(&[(P_SRC0, 1.0), (P_SMP0, 1.0)], 1, &[(36, 3)], 0.3);
    let layer = render(&[(P_SRC0, 2.0), (P_SMP0, 1.0)], 1, &[(36, 3)], 0.3);
    let (es, ep, el) = (db(probes::rms(&synth[tail.clone()])), db(probes::rms(&sample[tail.clone()])), db(probes::rms(&layer[tail])));
    eprintln!("synth {es:.1} sample {ep:.1} layer {el:.1} dB");
    assert!(el > es.max(ep) + 0.5, "layer must be louder than either part");
    assert!(hz(&synth[3 * 512 + 480..3 * 512 + 4800]) < 300.0, "Synth mode played the 1 kHz sample");
}

#[test]
fn closed_hat_chokes_open_hat_sample() {
    let p = [(P_SRC0 + 2, 1.0), (P_SRC0 + 3, 1.0)];
    let open_only = render(&p, 2, &[(46, 3)], 1.0);
    let choked = render(&p, 2, &[(46, 3), (42, 20)], 1.0);
    // After the closed hat itself (0.21 s + 0.5 s file) has ended.
    let late = (SR * 0.75) as usize..(SR * 0.95) as usize;
    let (o, c) = (db(probes::rms(&open_only[late.clone()])), db(probes::rms(&choked[late])));
    assert!(o > -40.0 && c < o - 30.0, "open {o:.1} dB vs after choke {c:.1} dB");
}

#[global_allocator]
static ALLOC: sdsp_test_kit::alloc::CountingAllocator = sdsp_test_kit::alloc::CountingAllocator;

#[test]
fn sample_pads_do_not_allocate() {
    sdsp_test_kit::alloc::reset();
    let notes: Vec<(u8, usize)> = (3..40).map(|b| ([36u8, 42, 46, 38][b % 4], b)).collect();
    let _ = render(&[(P_SRC0, 1.0), (P_SRC0 + 2, 2.0), (P_SRC0 + 3, 1.0)], 3, &notes, 0.45);
    sdsp_test_kit::alloc::assert_rt_clean("superduper-drum sample pads", 42);
}

#[test]
fn exported_kit_opens_in_the_sfz_engine() {
    // Build a shared state by hand: two sample pads, loaded paths set.
    let shared = superduper_drum::PluginShared::new();
    let kick = fixture().join("Fixture Kit").join("Kick_B.wav");
    let hat = fixture().join("Fixture Kit").join("Open_Hat_01.wav");
    shared.params[P_SRC0].store(1.0, Ordering::Relaxed);
    shared.params[P_SRC0 + 3].store(2.0, Ordering::Relaxed);
    *shared.pads.loaded_path[0].lock() = Some(kick);
    *shared.pads.loaded_path[3].lock() = Some(hat);
    let dir = fixture().join("kits");
    let file = pads::save_kit_sfz(&shared, &dir, "Test Kit").expect("save");
    let inst = superduper_synth_core::sfz::load::load_instrument(&file, 2, None).expect("load kit");
    assert!(inst.missing.is_empty(), "{:?}", inst.missing);
    let keys: Vec<i32> = inst.def.regions.iter().map(|r| r.lokey).collect();
    assert_eq!(keys, vec![36, 46]);
    assert_eq!(inst.def.regions[1].off_by, Some(1), "open hat is choked by the closed-hat group");
}

#[test]
fn kit_from_pack_fills_what_the_pack_has_and_kit_files_load_back() {
    std::env::set_var("SUPERDUPER_DRUM_ROOT", fixture());
    let shared = superduper_drum::PluginShared::new();
    pads::rescan(&shared);
    assert_eq!(pads::packs(&shared), vec!["Fixture Kit".to_string()]);
    pads::kit_from_pack(&shared, "Fixture Kit", None);
    let src = |p: usize| shared.params[P_SRC0 + p].load(Ordering::Relaxed).round() as u8;
    // Fixture has kicks and both hats, no snare / clap / perc.
    assert_eq!((src(0), src(1), src(2), src(3), src(4), src(5)), (1, 0, 1, 1, 0, 0));

    // A kit file written elsewhere: GM aliases (35 kick, 44 pedal hat) land on the right pads.
    let dir = fixture().join("kits2");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let kick = fixture().join("Fixture Kit").join("Kick_A.wav");
    std::fs::write(dir.join("k.sfz"), format!("<region> key=35 tune=-200 volume=-6 sample={}\n<region> key=44 sample=../Fixture Kit/Closed_Hat_01.wav\n", kick.display())).expect("w");
    let n = pads::load_kit_sfz(&shared, &dir.join("k.sfz")).expect("load");
    assert_eq!(n, 2);
    assert_eq!(*shared.pads.requested_path[0].lock(), Some(kick));
    assert!(shared.pads.requested_path[2].lock().as_ref().is_some_and(|p| p.ends_with("Closed_Hat_01.wav")));
    assert!((shared.params[superduper_drum::voice_param_idx(0, 0)].load(Ordering::Relaxed) + 2.0).abs() < 1e-4, "tune −200 ct");
    assert!((shared.params[superduper_drum::voice_param_idx(0, 2)].load(Ordering::Relaxed) - 0.501).abs() < 0.01, "volume −6 dB");
    assert_eq!(src(3), 0, "pads the kit does not name go back to synth");
}

#[test]
fn agent_builds_a_kit_with_params_only() {
    // Through the host, the way MCP / automation would: Kit Pack = 1 (the
    // only fixture pack), then a kick note plays the pack's first kick.
    use superduper_drum::{P_KIT_DICE, P_KIT_PACK};
    let l = render(&[(P_KIT_PACK, 1.0), (P_KIT_DICE, 0.0)], 3, &[(36, 6)], 0.5);
    let win = 6 * 512 + 480..6 * 512 + 4800;
    let before = db(probes::rms(&l[3 * 512..6 * 512]));
    assert!(before < -90.0, "filling a kit auditioned pads by itself: {before:.1} dB before the note");
    assert!((hz(&l[win.clone()]) - 500.0).abs() < 15.0, "Kick_A from the pack: {:.0} Hz", hz(&l[win]));

    // Reopening a project must not rebuild the kit: marking params applied
    // and stepping the loader leaves a hand-set pad alone.
    std::env::set_var("SUPERDUPER_DRUM_ROOT", fixture());
    let shared = superduper_drum::PluginShared::new();
    pads::rescan(&shared);
    shared.params[P_KIT_PACK].store(1.0, Ordering::Relaxed);
    shared.params[P_SMP0].store(1.0, Ordering::Relaxed);
    pads::mark_kit_applied(&shared);
    pads::apply_kit_params(&shared);
    assert_eq!(shared.params[P_SMP0].load(Ordering::Relaxed), 1.0, "saved pick kept");
    shared.params[P_KIT_DICE].store(7.0, Ordering::Relaxed);
    pads::apply_kit_params(&shared);
    assert_eq!(shared.params[P_SRC0].load(Ordering::Relaxed), 1.0, "a new dice value does rebuild");
}


#[test]
fn nothing_sounds_before_a_note() {
    // The synth Clap used to fire once on the first block of every instance.
    let l = render(&[], 0, &[], 0.1);
    assert!(db(probes::rms(&l)) < -200.0, "{:.1} dB with no notes", db(probes::rms(&l)));
}

#[test]
fn key_moves_tonal_pads_to_the_song_key() {
    use superduper_drum::P_KEY;
    // Fixture names carry keys: Kick_A = A (500 Hz tone), Kick_B = B.
    let win = 3 * 512 + 480..3 * 512 + 9600;
    let off = render(&[(P_SRC0, 1.0), (P_SMP0, 0.0), (P_KEY, 0.0)], 1, &[(36, 3)], 0.4);
    let in_c = render(&[(P_SRC0, 1.0), (P_SMP0, 0.0), (P_KEY, 1.0)], 1, &[(36, 3)], 0.4);
    assert!((hz(&off[win.clone()]) - 500.0).abs() < 15.0, "Key off leaves it: {:.0}", hz(&off[win.clone()]));
    // A → C is +3 st: 500 · 2^(3/12) = 594.6 Hz.
    assert!((hz(&in_c[win.clone()]) - 594.6).abs() < 15.0, "Key C: {:.0} Hz", hz(&in_c[win.clone()]));

    // Auto reads the song-key bus file.
    let bus = fixture().join("song-key");
    std::fs::write(&bus, "D minor\n").expect("w");
    std::env::set_var("SUPERDUPER_SONG_KEY", &bus);
    let auto = render(&[(P_SRC0, 1.0), (P_SMP0, 0.0), (P_KEY, 13.0)], 1, &[(36, 3)], 0.4);
    std::env::remove_var("SUPERDUPER_SONG_KEY");
    // A → D is +5 st: 667.4 Hz.
    assert!((hz(&auto[win.clone()]) - 667.4).abs() < 15.0, "Key Auto (D): {:.0} Hz", hz(&auto[win]));
}

#[test]
fn key_retunes_the_synth_kick_too() {
    use superduper_drum::P_KEY;
    let tail = |l: &[f32]| superduper_synth_core::drumlib::drum_pitch_hz(&l[3 * 512..], SR as f32).unwrap_or(0.0);
    let off = render(&[(P_KEY, 0.0)], 0, &[(36, 3)], 0.6);
    let in_c = render(&[(P_KEY, 1.0)], 0, &[(36, 3)], 0.6);
    // The synth kick is A (55 Hz); in C it is +3 st = 65.4 Hz.
    assert!((tail(&off) - 55.0).abs() < 2.0, "synth kick {:.1} Hz", tail(&off));
    assert!((tail(&in_c) - 65.4).abs() < 2.5, "synth kick in C {:.1} Hz", tail(&in_c));
}
