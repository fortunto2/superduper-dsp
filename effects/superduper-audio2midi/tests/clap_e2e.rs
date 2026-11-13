//! End to end through the real CLAP plugin: play plucks with the transport running (the plugin
//! records), stop (Auto analyses), jump back and play again with Thru 0 (the plugin replays the
//! notes as note events, the original is silent). The counting allocator watches every
//! `process()` of the whole session, capture, take close, note-set swap and replay included.

use clack_common::events::{event_types::ParamValueEvent, Pckn};
use clack_common::utils::{ClapId, Cookie};
use sdsp_test_kit::{render_session, PluginUnderTest, TransportSpec};
use std::sync::atomic::Ordering;
use superduper_audio2midi::{worker::ANALYSES_COMPLETED, P_AUTO, P_CAPTURE, P_THRU};

struct Target;
impl PluginUnderTest for Target {
    type Plugin = superduper_audio2midi::SuperDuperAudio2Midi;
    const ID: &'static std::ffi::CStr = c"co.superduperai.audio2midi";
}

#[global_allocator]
static ALLOC: sdsp_test_kit::alloc::CountingAllocator = sdsp_test_kit::alloc::CountingAllocator;

const SR: f64 = 48_000.0;
const BLOCK: usize = 512;
const SONG_START: f64 = 10.0;

fn pluck(buf: &mut [f32], start: f64, midi: u8, dur: f64) {
    let f = 440.0 * 2f64.powf((midi as f64 - 69.0) / 12.0);
    let n = (SR / f).round() as usize;
    let mut line: Vec<f32> = (0..n).map(|i| (((i * 7919 + midi as usize * 104729) % 2001) as f32 / 1000.0) - 1.0).collect();
    for i in 1..n {
        line[i] = 0.5 * (line[i] + line[i - 1]);
    }
    let (s0, len) = ((start * SR) as usize, (dur * SR) as usize);
    let mut idx = 0;
    for k in 0..len {
        let Some(slot) = buf.get_mut(s0 + k) else { break };
        *slot += line[idx] * 0.6;
        line[idx] = 0.5 * (line[idx] + line[(idx + 1) % n]) * 0.9985;
        idx = (idx + 1) % n;
    }
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

#[test]
fn records_analyses_and_replays_as_midi() {
    let out_dir = std::env::temp_dir().join(format!("sdsp-a2m-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out_dir);
    std::env::set_var("SUPERDUPER_A2M_OUT", &out_dir);

    let plucks: Vec<(f64, u8)> = [40u8, 45, 50, 55, 59, 64].iter().enumerate().map(|(i, &m)| (0.5 + i as f64 * 0.9, m)).collect();
    let n1 = ((7.5 * SR) as usize / BLOCK) * BLOCK; // capture phase
    let n2 = n1 + 4 * BLOCK; // stopped, then the replay phase starts here
    let total = n2 + n1 + 8 * BLOCK;
    let mut audio = vec![0.0f32; n1];
    for &(t, m) in &plucks {
        pluck(&mut audio, t, m, 1.6);
    }
    let peak = audio.iter().fold(0.0f32, |a, b| a.max(b.abs()));
    audio.iter_mut().for_each(|x| *x *= 0.7 / peak);
    let mut input = audio.clone();
    input.resize(total, 0.0);
    // the replay phase carries the same audio again: Thru 0 must silence it
    input[n2..n2 + n1].copy_from_slice(&audio);

    let before = ANALYSES_COMPLETED.load(Ordering::SeqCst);
    sdsp_test_kit::alloc::reset();
    let (out_l, _out_r, notes) = render_session::<Target>(
        SR,
        BLOCK,
        &input,
        &input,
        |pos, buf| {
            if pos == n2 {
                for (id, v) in [(P_CAPTURE, 0.0), (P_AUTO, 0.0), (P_THRU, 0.0)] {
                    buf.push(&ParamValueEvent::new(0, ClapId::new(id as u32), Pckn::match_all(), v, Cookie::empty()));
                }
            }
        },
        |pos| {
            if pos < n1 {
                Some(TransportSpec { playing: true, pos_seconds: SONG_START + pos as f64 / SR, tempo: 100.0 })
            } else if pos < n2 {
                if pos == n2 - BLOCK {
                    // let the worker finish the Auto analysis the stop triggered
                    let t0 = std::time::Instant::now();
                    while ANALYSES_COMPLETED.load(Ordering::SeqCst) == before && t0.elapsed().as_secs() < 90 {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                }
                Some(TransportSpec { playing: false, pos_seconds: SONG_START + n1 as f64 / SR, tempo: 100.0 })
            } else {
                Some(TransportSpec { playing: true, pos_seconds: SONG_START + (pos - n2) as f64 / SR, tempo: 100.0 })
            }
        },
    );
    assert_eq!(ANALYSES_COMPLETED.load(Ordering::SeqCst), before + 1, "exactly one analysis (the stop)");

    // Thru: audio passed while capturing, silent while the notes replay
    assert!(rms(&out_l[2 * BLOCK..n1]) > 0.02, "capture-phase output is silent");
    assert!(rms(&out_l[n2 + 2 * BLOCK..n2 + n1]) < 1e-4, "Thru 0 still passes audio: {}", rms(&out_l[n2 + 2 * BLOCK..n2 + n1]));

    // the replay: every pluck comes back as a note-on of the same key at the same song position
    let ons: Vec<_> = notes.iter().filter(|n| n.on).collect();
    eprintln!("{} note-ons, {} note-offs", ons.len(), notes.len() - ons.len());
    for &(t, m) in &plucks {
        let want = n2 + (t * SR) as usize;
        let hit = ons.iter().find(|n| n.key == m as u16 && n.frame.abs_diff(want) < (0.07 * SR) as usize);
        assert!(hit.is_some(), "pluck {m} at {t}s not replayed; note-ons: {:?}", ons.iter().map(|n| (n.key, n.frame as f64 / SR)).collect::<Vec<_>>());
        assert!(hit.unwrap().velocity > 0.0 && hit.unwrap().velocity <= 1.0);
    }
    assert!(notes.iter().all(|n| n.channel == 0), "Any mode plays on channel 0");
    // nothing left hanging at the end of the session: every on has an off
    let offs = notes.len() - ons.len();
    assert_eq!(ons.len(), offs, "{} ons vs {offs} offs", ons.len());

    // exported file: song-time stamped, at the transport's tempo
    let mids: Vec<_> = std::fs::read_dir(&out_dir).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "mid")).collect();
    assert_eq!(mids.len(), 1, "one .mid in {}", out_dir.display());
    assert!(out_dir.read_dir().unwrap().flatten().any(|e| e.path().extension().is_some_and(|x| x == "json")));
    let ev = superduper_transcribe::midi::read_smf_events(&std::fs::read(mids[0].path()).unwrap()).unwrap();
    let first_on = ev.iter().find(|e| e.4).unwrap();
    assert!((first_on.0 - (SONG_START + 0.5)).abs() < 0.1, "first note at {} s", first_on.0);
    let _ = std::fs::remove_dir_all(&out_dir);

    sdsp_test_kit::alloc::assert_rt_clean("superduper-audio2midi", total / BLOCK);
}
