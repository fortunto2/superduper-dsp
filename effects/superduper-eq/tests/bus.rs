//! End to end: a running plugin shows up on the SuperDuper bus with what it
//! is actually passing. A 1 kHz tone through the EQ (flat) must land in the
//! band that contains 1 kHz, at about its level.

use sdsp_test_kit::host::render_with;
use sdsp_test_kit::PluginUnderTest;
use superduper_dsp_sdk::bus::{band_edges, Reader};

struct Target;
impl PluginUnderTest for Target {
    type Plugin = superduper_eq::SuperDuperEq;
    const ID: &'static std::ffi::CStr = c"co.superduperai.eq";
}

#[test]
fn eq_publishes_its_spectrum_on_the_bus() {
    let bus = std::env::temp_dir().join(format!("sdsp-bus-eq-{}", std::process::id()));
    let _ = std::fs::remove_file(&bus);
    std::env::set_var("SUPERDUPER_BUS", &bus);
    let sr = 48_000.0;
    let n = (sr * 1.0) as usize;
    let tone: Vec<f32> = (0..n).map(|i| 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / sr as f32).sin()).collect();
    let seen = std::sync::Mutex::new(None);
    let _ = render_with::<Target>(sr, 512, &tone, &tone, |pos, _| {
        // Halfway through, hold the audio thread so the publisher (20×/s)
        // gets a turn, then read the bus like any other consumer would.
        if pos == 512 * 40 {
            std::thread::sleep(std::time::Duration::from_millis(200));
            let rows = Reader::open().expect("bus").read_all();
            *seen.lock().unwrap() = rows.into_iter().find(|r| r.plugin == "SuperDuper EQ");
        }
    });
    let row = seen.into_inner().unwrap().expect("the EQ never appeared on the bus");
    let edges = band_edges();
    let loudest = (0..row.bands_db.len()).max_by(|&a, &b| row.bands_db[a].total_cmp(&row.bands_db[b])).unwrap();
    eprintln!("rms {:.1} dB, loudest band {loudest} ({:.0}-{:.0} Hz) at {:.1} dB", row.rms_db, edges[loudest], edges[loudest + 1], row.bands_db[loudest]);
    assert!(edges[loudest] <= 1000.0 && 1000.0 < edges[loudest + 1], "1 kHz tone, loudest band {:.0}-{:.0} Hz", edges[loudest], edges[loudest + 1]);
    // A 0.5-amplitude sine is −9 dB RMS, and all of it sits in one band.
    assert!((row.rms_db + 9.0).abs() < 0.5, "rms {:.1}", row.rms_db);
    // Band levels are band RMS, so they add up (in power) to the total. A
    // tone 26 Hz above a band edge spills part of its window into the
    // neighbour, so check the sum rather than one band.
    let total = 10.0 * row.bands_db.iter().map(|b| 10f32.powf(b / 10.0)).sum::<f32>().log10();
    eprintln!("sum of bands {total:.2} dB");
    assert!((total - row.rms_db).abs() < 0.5, "bands sum to {total:.1} dB, total is {:.1}", row.rms_db);
    let others = row.bands_db.iter().enumerate().filter(|(i, _)| (*i as i32 - loudest as i32).abs() > 1).fold(-200f32, |m, (_, v)| m.max(*v));
    assert!(others < -40.0, "energy leaked into far bands: {others:.1} dB");
}
