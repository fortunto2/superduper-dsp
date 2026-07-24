//! Headless sample selection — the fix that lets host automation / MCP /
//! producer-pal pick a sample by moving the `Sample` param, with NO GUI.
//!
//! Before this, `active_sample` was only ever swapped by the GUI dropdown
//! (`pick_sample`), so a freshly-instantiated Sampler driven purely over the
//! wire stayed empty (silent) and setting the `Sample` param did nothing.
//! `maybe_load_pending_sample` (called from `on_main_thread` + the main-thread
//! param flush) closes that gap. This test drives the exact off-GUI path.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use superduper_sampler::{maybe_load_pending_sample, refresh_library, PluginShared, P_SAMPLE};

/// Write a minimal 16-bit PCM mono WAV (a short sine) the sampler can decode.
fn write_tone_wav(path: &Path, hz: f32, secs: f32, sr: u32) {
    let n = (secs * sr as f32) as usize;
    let mut pcm = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f32 / sr as f32;
        let s = (2.0 * std::f32::consts::PI * hz * t).sin() * 0.5;
        pcm.extend_from_slice(&((s * i16::MAX as f32) as i16).to_le_bytes());
    }
    let data_len = pcm.len() as u32;
    let byte_rate = sr * 2; // channels(1) * bits(16)/8
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data_len).to_le_bytes()).unwrap();
    f.write_all(b"WAVE").unwrap();
    f.write_all(b"fmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap(); // fmt chunk size
    f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
    f.write_all(&1u16.to_le_bytes()).unwrap(); // mono
    f.write_all(&sr.to_le_bytes()).unwrap();
    f.write_all(&byte_rate.to_le_bytes()).unwrap();
    f.write_all(&2u16.to_le_bytes()).unwrap(); // block align
    f.write_all(&16u16.to_le_bytes()).unwrap(); // bits
    f.write_all(b"data").unwrap();
    f.write_all(&data_len.to_le_bytes()).unwrap();
    f.write_all(&pcm).unwrap();
}

fn unique_tmp() -> PathBuf {
    std::env::temp_dir().join(format!("sdsp_sampler_headless_{}", std::process::id()))
}

#[test]
fn headless_param_change_loads_the_sample() {
    // Isolated sample root with one decodable WAV.
    let root = unique_tmp();
    let pack = root.join("testpack");
    std::fs::create_dir_all(&pack).unwrap();
    write_tone_wav(&pack.join("tone.wav"), 220.0, 0.1, 44_100);

    let shared = PluginShared::new();
    // Point the sampler at our temp root only, then scan.
    *shared.inner.sample_roots.lock() = vec![root.clone()];
    let count = refresh_library(&shared.inner);
    assert_eq!(count, 1, "library should have found exactly our one WAV");

    // Nothing loaded yet — the fresh-instance silent state.
    assert_eq!(shared.inner.current_index.load(Ordering::Relaxed), -1);
    assert!(shared.inner.active_sample.lock().samples.is_empty());

    // Simulate the host / MCP moving the Sample param to index 0.
    shared.inner.params[P_SAMPLE].store(0.0, Ordering::Relaxed);
    // The off-GUI decode path (what on_main_thread / flush now call).
    maybe_load_pending_sample(&shared.inner);

    assert_eq!(
        shared.inner.current_index.load(Ordering::Relaxed),
        0,
        "Sample param should have driven a load of index 0"
    );
    assert!(
        !shared.inner.active_sample.lock().samples.is_empty(),
        "active_sample must now hold decoded audio, not the empty default"
    );

    // Out-of-range automation value clamps to the last sample, not silence.
    shared.inner.params[P_SAMPLE].store(999.0, Ordering::Relaxed);
    maybe_load_pending_sample(&shared.inner);
    assert_eq!(
        shared.inner.current_index.load(Ordering::Relaxed),
        0,
        "out-of-range index should clamp to the only sample (0)"
    );

    let _ = std::fs::remove_dir_all(&root);
}
