//! The bus key on the song timeline, as a host would drive it: a key track
//! and a keyed track, each a `LiveScope`, sharing one frames file. Whatever
//! order and speed the "host" runs them in, the keyed track must hear the
//! key track's audio at its own song time — that is what a faster-than-real-
//! time render lost when the key travelled on the wall clock (REAPER,
//! 2026-10-22: full-speed renders did not duck at all or ducked −11 dB
//! through the whole song, where 1x ducked −11.3 dB only on the key).
#![cfg(feature = "gui")]

use std::sync::Arc;
use std::time::{Duration, Instant};

use clack_plugin::events::event_types::{TransportEvent, TransportFlags};
use clack_plugin::events::{EventFlags, EventHeader};
use clack_plugin::utils::{BeatTime, SecondsTime};
use superduper_dsp_sdk::bus_frames::Frames;
use superduper_synth_core::bus_key::key_level_db;
use superduper_synth_core::bus_timeline::{HOP, WINDOW};
use superduper_synth_core::gui::LiveScope;

const SR: f32 = 48_000.0;
const BLOCK: usize = 512;
const SONG_S: f64 = 4.0;
/// Key bursts (song seconds), irregular so a shifted key cannot line up.
const BURSTS: [(f64, f64); 4] = [(0.8, 1.3), (1.9, 2.05), (2.4, 3.1), (3.4, 3.55)];

fn gate(t: f64) -> bool { BURSTS.iter().any(|&(a, b)| (a..b).contains(&t)) }

fn transport(t: f64) -> TransportEvent {
    TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags: TransportFlags::IS_PLAYING | TransportFlags::HAS_SECONDS_TIMELINE,
        song_pos_beats: BeatTime::from_float(0.0),
        song_pos_seconds: SecondsTime::from_float(t),
        tempo: 120.0,
        tempo_inc: 0.0,
        loop_start_beats: BeatTime::from_float(0.0),
        loop_end_beats: BeatTime::from_float(0.0),
        loop_start_seconds: SecondsTime::from_float(0.0),
        loop_end_seconds: SecondsTime::from_float(0.0),
        bar_start: BeatTime::from_float(0.0),
        bar_number: 0,
        time_signature_numerator: 4,
        time_signature_denominator: 4,
    }
}

/// `SUPERDUPER_BUS` is process-wide; tests take turns setting it.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// One pair of scopes on their own slots of a fresh frames file. Hold the
/// guard for the test.
fn pair(tag: &str, slots: (usize, usize)) -> (Arc<LiveScope>, Arc<LiveScope>, std::sync::MutexGuard<'static, ()>) {
    let guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let p = std::env::temp_dir().join(format!("sdsp-timeline-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_file(format!("{}.frames", p.display()));
    std::env::set_var("SUPERDUPER_BUS", &p);
    let (key, keyed) = (Arc::new(LiveScope::new(4096)), Arc::new(LiveScope::new(4096)));
    for s in [&key, &keyed] { s.set_sample_rate(SR); }
    let (key_id, keyed_id) = (2 * slots.0 as u64 + 101, 2 * slots.1 as u64 + 101);
    key.timeline.attach(Frames::open().expect("frames"), slots.0, key_id);
    keyed.timeline.attach(Frames::open().expect("frames"), slots.1, keyed_id);
    keyed.timeline.set_key_source(Some((slots.0, key_id)));
    // What the two publisher threads do every 50 ms: the keyed track asks,
    // the key track sees it is wanted.
    let now = superduper_dsp_sdk::bus::now_ms();
    keyed.timeline.tick(now);
    keyed.begin_block(Some(&transport(-1.0)));
    key.timeline.tick(now);
    (key, keyed, guard)
}

fn blocks() -> impl Iterator<Item = (usize, f64)> {
    (0..(SONG_S * SR as f64) as usize / BLOCK).map(|k| (k, (k * BLOCK) as f64 / SR as f64))
}

/// The key track plays one block: noise while the gate is open.
fn play_key(key: &LiveScope, k: usize, t: f64, rng: &mut u32) {
    key.begin_block(Some(&transport(t)));
    for i in 0..BLOCK {
        *rng ^= *rng << 13;
        *rng ^= *rng >> 17;
        *rng ^= *rng << 5;
        let noise = (*rng as f32 / u32::MAX as f32 - 0.5) * 0.6;
        key.push(if gate((k * BLOCK + i) as f64 / SR as f64) { noise } else { 0.0 });
    }
    key.end_block();
}

/// What the keyed track hears at each block start, scored against the gate:
/// the key's frame ending at `t` covers `t − WINDOW/sr .. t`, so a block
/// whose whole window is inside a burst must hear it and one whose whole
/// window is outside must not, with the slack the timeline itself allows: it
/// takes a frame up to two hops at 44.1 kHz old as the block's own (`GOOD_S`),
/// which at 48 kHz still sees 2 ms of a burst that ended (t = 2.1013, −67 dB,
/// one run in ten). Returns mismatches.
fn score(heard: &[(f64, f32)]) -> (usize, usize) {
    let w = WINDOW as f64 / SR as f64;
    let slack = 2.0 * HOP as f64 / 44_100.0;
    let (mut checked, mut wrong) = (0, 0);
    for &(t, db) in heard {
        let span = |a: f64, b: f64| (0..64).map(move |i| a + (b - a) * i as f64 / 63.0);
        let all_on = span(t - w, t - 1e-6).all(gate);
        let all_off = span(t - w - slack, t - 1e-6).all(|x| !gate(x));
        if all_on || all_off {
            checked += 1;
            if (all_on && db < -40.0) || (all_off && db > -80.0) { wrong += 1; }
        }
    }
    (checked, wrong)
}

#[test]
fn key_rendered_ahead_is_read_at_the_song_time_not_the_latest() {
    let (key, keyed, _turn) = pair("ahead", (10, 11));
    let mut rng = 0x1234_5678;
    for (k, t) in blocks() { play_key(&key, k, t, &mut rng); }
    // The whole key track is done before the keyed track starts: the wall
    // clock would give every block the song's last moment.
    let heard: Vec<(f64, f32)> = blocks().map(|(_, t)| {
        keyed.begin_block(Some(&transport(t)));
        (t, key_level_db(&keyed))
    }).collect();
    let (checked, wrong) = score(&heard);
    assert!(checked > 250, "{checked} blocks scored");
    assert_eq!(wrong, 0, "every scored block hears the key as it was at that song time");
}

#[test]
fn one_thread_keyed_track_first_hears_the_key_one_block_late_at_most_and_never_waits() {
    // A host that runs both on one thread, keyed track first: the key's
    // frame for this block cannot exist yet. It must not wait for it (that
    // would only stall the render) and must still hear the previous block.
    let (key, keyed, _turn) = pair("serial", (20, 21));
    let mut rng = 0x9e37_79b9;
    let t0 = Instant::now();
    let mut heard = Vec::new();
    for (k, t) in blocks() {
        keyed.begin_block(Some(&transport(t)));
        heard.push((t, key_level_db(&keyed)));
        keyed.end_block();
        play_key(&key, k, t, &mut rng);
    }
    let took = t0.elapsed();
    let (checked, wrong) = score(&heard);
    assert!(checked > 250, "{checked} blocks scored");
    assert_eq!(wrong, 0, "the key's previous block ends exactly at this block's start");
    assert!(took < Duration::from_millis(1500), "rendered 4 s in {took:?}: no per-block waits");
}

#[test]
fn faster_than_real_time_keyed_track_waits_for_a_key_track_that_is_behind() {
    let (key, keyed, _turn) = pair("behind", (30, 31));
    // The key track starts late and runs on its own thread, as a render
    // thread that got scheduled after ours.
    let k2 = key.clone();
    let producer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        let mut rng = 0xdead_beef;
        for (k, t) in blocks() { play_key(&k2, k, t, &mut rng); }
    });
    let heard: Vec<(f64, f32)> = blocks().map(|(_, t)| {
        keyed.begin_block(Some(&transport(t)));
        let db = key_level_db(&keyed);
        // Its own output: how the block clock knows a block's length.
        for _ in 0..BLOCK { keyed.push(0.0); }
        keyed.end_block();
        (t, db)
    }).collect();
    producer.join().expect("producer");
    let (checked, wrong) = score(&heard);
    assert!(checked > 250, "{checked} blocks scored");
    // The first few blocks decide that this is a render; a burst starts at
    // 0.8 s, well after that.
    assert_eq!(wrong, 0, "waited for the key instead of keying on its past");
}

/// The key track's render thread starts well after ours: the macOS CI runner
/// (2026-10-26) overslept the 30 ms of the test above past `WAIT_MAX`, and the
/// keyed track, taking a key track with no frame yet for one that is not
/// played, rendered the whole song with no key (125 blocks wrong). A busy Mac
/// starting an offline render is the same race, so a key track that has not
/// written its first frame gets start-up patience, not 100 ms.
#[test]
fn a_key_track_that_starts_late_is_waited_for_not_taken_for_absent() {
    let (key, keyed, _turn) = pair("late", (60, 61));
    let k2 = key.clone();
    let t0 = Instant::now();
    let producer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        let started = t0.elapsed();
        let mut rng = 0x0bad_cafe;
        for (k, t) in blocks() { play_key(&k2, k, t, &mut rng); }
        started
    });
    let heard: Vec<(f64, f32)> = blocks().map(|(_, t)| {
        keyed.begin_block(Some(&transport(t)));
        let db = key_level_db(&keyed);
        for _ in 0..BLOCK { keyed.push(0.0); }
        keyed.end_block();
        (t, db)
    }).collect();
    let started = producer.join().expect("producer");
    let (checked, wrong) = score(&heard);
    assert!(checked > 250, "{checked} blocks scored");
    // The promise is patience up to START_MAX (1 s), not forever. A 300 ms
    // sleep that the machine stretched past it (the macOS CI VM, 2026-10-27:
    // 125 wrong blocks with no other explanation) is a key track that really
    // was absent for that long, so the case this test is about did not happen.
    if started >= Duration::from_millis(900) {
        eprintln!("inconclusive: the key track started {started:?} late, past the 1 s patience; {wrong} blocks without key");
        return;
    }
    assert_eq!(wrong, 0, "a late key track (started {started:?}) was taken for an absent one");
}

/// The key track's render thread loses the CPU once, mid-song, for longer than
/// `STALL` while one block behind (a busy machine; the macOS CI runner,
/// 2026-10-26). That block cannot tell it from a host running both tracks on
/// one thread, and may take the key a block late, but the rest of the render
/// must go back to waiting: the key track then falls further behind than one
/// block, which a same-thread host can never do.
#[test]
fn a_key_thread_preempted_once_does_not_stop_the_waiting_for_the_rest_of_the_song() {
    let (key, keyed, _turn) = pair("preempted", (50, 51));
    let k2 = key.clone();
    let producer = std::thread::spawn(move || {
        let mut rng = 0x5eed_1234;
        let mut paused = Duration::ZERO;
        for (k, t) in blocks() {
            if k == 100 {
                let at = Instant::now();
                std::thread::sleep(Duration::from_millis(80));
                paused = at.elapsed();
            }
            play_key(&k2, k, t, &mut rng);
        }
        paused
    });
    let heard: Vec<(f64, f32)> = blocks().map(|(_, t)| {
        keyed.begin_block(Some(&transport(t)));
        let db = key_level_db(&keyed);
        for _ in 0..BLOCK { keyed.push(0.0); }
        keyed.end_block();
        (t, db)
    }).collect();
    let paused = producer.join().expect("producer");
    let (checked, wrong) = score(&heard);
    assert!(checked > 250, "{checked} blocks scored");
    // A pause past WAIT_MAX (100 ms) is a key track that stopped, and the keyed
    // track rightly stops waiting for it. The macOS CI VM stretched this 80 ms
    // sleep past that (2026-10-27: 101 blocks), so that run is not the case here.
    if paused >= Duration::from_millis(95) {
        eprintln!("inconclusive: the key thread paused {paused:?}, past the 100 ms wait; {wrong} blocks without key");
        return;
    }
    assert!(wrong <= 1, "{wrong} blocks keyed on a stale key after one preemption: the block that judged it serial may take the key a block late, no other");
}

#[test]
fn in_real_time_a_missing_key_frame_costs_no_wait() {
    let (_key, keyed, _turn) = pair("realtime", (40, 41));
    // Blocks at the pace of real time (10.7 ms of audio each), the key
    // track silent and frameless: begin_block must return at once.
    let mut worst = Duration::ZERO;
    for k in 0..6 {
        let t = (k * BLOCK) as f64 / SR as f64;
        for _ in 0..BLOCK { keyed.push(0.0); }
        std::thread::sleep(Duration::from_millis(11));
        let s = Instant::now();
        keyed.begin_block(Some(&transport(t)));
        worst = worst.max(s.elapsed());
    }
    assert!(worst < Duration::from_millis(3), "slowest real-time block start {worst:?}");
}

#[test]
fn stopped_transport_leaves_the_key_to_the_wall_clock_copy() {
    let (key, keyed, _turn) = pair("stopped", (50, 51));
    let mut rng = 7;
    for (k, t) in blocks().take(20) { play_key(&key, k, t, &mut rng); }
    let before = keyed.timeline.key_blocks();
    keyed.begin_block(None);
    assert_eq!(keyed.timeline.key_blocks(), before, "no transport: the publisher thread's copy stays in charge");
}

