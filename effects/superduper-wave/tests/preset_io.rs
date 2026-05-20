//! End-to-end tests for Wave's user-preset persistence:
//! - JSON schema round-trip via `PresetRepo`
//! - sibling `.wav` write / drop-load is sample-accurate
//! - the written WAV header matches Serum / Vital conventions
//!   (fmt tag 3, 32-bit IEEE float, mono, sample rate metadata)
//!
//! Tests use a per-run scratch directory under `/tmp` so they don't
//! touch the user's real `~/.superduper-dsp/wave/`.

use std::path::PathBuf;
use std::time::SystemTime;

use superduper_synth_core::user_preset::{PresetExtra, PresetName, UserPreset};
use superduper_synth_core::wav::{
    parse_wav_file, read_single_cycle_wav, write_mono_f32_wav, SINGLE_CYCLE_SAMPLE_RATE,
};
use superduper_wave::osc::WT_SIZE;
use superduper_wave::user_extra::{WaveExtra, WavePreset, WaveRepo};

fn scratch_home() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("sdsp-wave-test-{stamp}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Build a representative drawn curve — a sine wave that morphs into
/// a saw at the centre. Distinctive enough that bit-rot would show.
fn fixture_curve() -> Vec<f32> {
    (0..WT_SIZE)
        .map(|i| {
            let t = i as f32 / WT_SIZE as f32;
            let sine = (t * std::f32::consts::TAU).sin();
            let saw = 2.0 * t - 1.0;
            let mix = (t * 2.0).min(1.0);
            sine * (1.0 - mix) + saw * mix
        })
        .collect()
}

#[test]
fn preset_json_round_trip() {
    let home = scratch_home();
    std::env::set_var("HOME", &home);
    let repo: WaveRepo = WaveRepo::for_plugin("wave");
    let name = PresetName::new("Test Curve").unwrap();
    let frame_a = fixture_curve();
    let params = vec![0.5f32; 32]; // dummy; we won't check against PARAMS

    let preset: WavePreset = UserPreset::new(
        name.clone(),
        params.clone(),
        WaveExtra {
            frame_a: frame_a.clone(),
        },
    )
    .unwrap();
    repo.save(&preset, params.len()).unwrap();

    let listed = repo.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].as_str(), "Test Curve");

    let loaded = repo.load(&listed[0], params.len()).unwrap();
    assert_eq!(loaded.params, preset.params);
    assert_eq!(loaded.extra.frame_a.len(), WT_SIZE);
    for (a, b) in frame_a.iter().zip(loaded.extra.frame_a.iter()) {
        assert!((a - b).abs() < 1e-6, "JSON round-trip delta {}", a - b);
    }
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn wav_sibling_matches_serum_format() {
    let home = scratch_home();
    let frame_a = fixture_curve();
    let presets_dir = home.join("presets");
    std::fs::create_dir_all(&presets_dir).unwrap();
    let path = presets_dir.join("test.wav");
    write_mono_f32_wav(&path, &frame_a, SINGLE_CYCLE_SAMPLE_RATE).unwrap();

    // Inspect the on-disk header at byte level. Serum expects:
    //   - "RIFF" / "WAVE"
    //   - fmt tag = 3 (IEEE float)
    //   - channels = 1
    //   - bits per sample = 32
    //   - sample rate = (whatever we wrote)
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[12..16], b"fmt ");
    let fmt_tag = u16::from_le_bytes([bytes[20], bytes[21]]);
    assert_eq!(fmt_tag, 3, "fmt tag must be 3 (IEEE float)");
    let channels = u16::from_le_bytes([bytes[22], bytes[23]]);
    assert_eq!(channels, 1, "single-cycle WAV must be mono");
    let sample_rate =
        u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    assert_eq!(sample_rate, SINGLE_CYCLE_SAMPLE_RATE);
    let bits = u16::from_le_bytes([bytes[34], bytes[35]]);
    assert_eq!(bits, 32, "must be 32-bit float");

    // Parse via the public reader and check sample fidelity.
    let parsed = parse_wav_file(&path).unwrap();
    assert_eq!(parsed.samples.len(), WT_SIZE);
    for (a, b) in frame_a.iter().zip(parsed.samples.iter()) {
        assert!((a - b).abs() < 1e-7, "WAV round-trip delta {}", a - b);
    }
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn drag_drop_resamples_arbitrary_wav_to_wt_size() {
    let home = scratch_home();
    let path = home.join("source.wav");

    // 512-sample sine — what would happen if user drops a short sample.
    let src: Vec<f32> = (0..512)
        .map(|i| (i as f32 / 512.0 * std::f32::consts::TAU).sin())
        .collect();
    write_mono_f32_wav(&path, &src, 48_000).unwrap();

    let curve = read_single_cycle_wav(&path, WT_SIZE).unwrap();
    assert_eq!(curve.len(), WT_SIZE);
    // The resampled curve is still recognisably a sine — peak near
    // quarter-cycle, zero near start and half.
    assert!(curve[0].abs() < 0.05);
    assert!(curve[WT_SIZE / 2].abs() < 0.05);
    let peak_idx = curve
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .unwrap()
        .0;
    let expected = WT_SIZE / 4;
    assert!(
        (peak_idx as i32 - expected as i32).abs() < 8,
        "peak {peak_idx} should be near {expected}"
    );
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn extra_validation_rejects_wrong_length() {
    let bad = WaveExtra { frame_a: vec![0.0; 1024] }; // not WT_SIZE
    let err = bad.validate().unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("frame_a length"), "got: {msg}");
}

#[test]
fn extra_validation_rejects_nan() {
    let mut samples = fixture_curve();
    samples[123] = f32::NAN;
    let bad = WaveExtra { frame_a: samples };
    let err = bad.validate().unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("non-finite"), "got: {msg}");
}

#[test]
fn extra_validation_rejects_out_of_range() {
    let mut samples = fixture_curve();
    samples[42] = 9.99; // |sample| > 2.0
    let bad = WaveExtra { frame_a: samples };
    let err = bad.validate().unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("magnitude"), "got: {msg}");
}

#[test]
fn full_save_load_workflow_with_paired_wav() {
    // Simulates exactly what the GUI's "Save" button does.
    let home = scratch_home();
    std::env::set_var("HOME", &home);
    let repo: WaveRepo = WaveRepo::for_plugin("wave");

    let name = PresetName::new("Distorted Saw").unwrap();
    let frame_a = fixture_curve();
    let params = vec![0.42f32; 32];
    let preset = UserPreset {
        version: superduper_synth_core::user_preset::PRESET_FORMAT_VERSION,
        name: name.clone(),
        params: params.clone(),
        extra: WaveExtra { frame_a: frame_a.clone() },
    };

    // 1. Save JSON via repo
    repo.save(&preset, params.len()).unwrap();

    // 2. Save sibling .wav (mimics write_sibling_wav in gui.rs)
    let dir = repo.base_dir().join("presets");
    std::fs::create_dir_all(&dir).unwrap();
    let wav_path = dir.join(format!("{}.wav", name.as_str()));
    write_mono_f32_wav(&wav_path, &frame_a, SINGLE_CYCLE_SAMPLE_RATE).unwrap();

    // Both files should exist.
    let json_path = dir.join(format!("{}.json", name.as_str()));
    assert!(json_path.exists(), "JSON missing at {json_path:?}");
    assert!(wav_path.exists(), "WAV missing at {wav_path:?}");

    // 3. Load JSON back and confirm the curve survives
    let listed = repo.list();
    assert_eq!(listed.len(), 1);
    let loaded = repo.load(&listed[0], params.len()).unwrap();
    for (a, b) in frame_a.iter().zip(loaded.extra.frame_a.iter()) {
        assert!((a - b).abs() < 1e-6);
    }

    // 4. Also independently load the .wav and confirm it's identical
    let wav_curve = read_single_cycle_wav(&wav_path, WT_SIZE).unwrap();
    for (a, b) in frame_a.iter().zip(wav_curve.iter()) {
        assert!((a - b).abs() < 1e-7);
    }

    // 5. Auto-default — save_last + load_last
    repo.save_last(&preset, params.len()).unwrap();
    let last = repo.load_last(params.len()).unwrap();
    assert_eq!(last.name.as_str(), "Distorted Saw");
    assert_eq!(last.extra.frame_a.len(), WT_SIZE);

    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn corrupted_json_returns_error_not_panic() {
    let home = scratch_home();
    std::env::set_var("HOME", &home);
    let repo: WaveRepo = WaveRepo::for_plugin("wave");
    let dir = repo.base_dir().join("presets");
    std::fs::create_dir_all(&dir).unwrap();
    // Write garbage that's not valid JSON.
    std::fs::write(dir.join("Bad.json"), b"{not json}").unwrap();

    let name = PresetName::new("Bad").unwrap();
    let err = repo.load(&name, 16).unwrap_err();
    // Validation/parsing error — NOT a panic.
    let msg = format!("{err}");
    assert!(
        msg.contains("json") || msg.contains("JSON") || msg.contains("expected"),
        "got: {msg}"
    );
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn corrupted_last_json_falls_back_to_none() {
    let home = scratch_home();
    std::env::set_var("HOME", &home);
    let repo: WaveRepo = WaveRepo::for_plugin("wave");
    std::fs::create_dir_all(repo.base_dir()).unwrap();
    std::fs::write(repo.base_dir().join("last.json"), b"corrupt!").unwrap();
    assert!(
        repo.load_last(16).is_none(),
        "corrupt last.json should return None, not panic"
    );
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn future_format_version_rejected() {
    let home = scratch_home();
    std::env::set_var("HOME", &home);
    let repo: WaveRepo = WaveRepo::for_plugin("wave");
    let dir = repo.base_dir().join("presets");
    std::fs::create_dir_all(&dir).unwrap();

    // Hand-write a JSON with version 999.
    let params_vec: Vec<f32> = vec![0.5; 16];
    let frame_a: Vec<f32> = (0..WT_SIZE).map(|_| 0.0f32).collect();
    let bad_json = serde_json::json!({
        "version": 999,
        "name": "Future",
        "params": params_vec,
        "extra": { "frame_a": frame_a }
    });
    std::fs::write(
        dir.join("Future.json"),
        serde_json::to_string(&bad_json).unwrap(),
    )
    .unwrap();

    let name = PresetName::new("Future").unwrap();
    let err = repo.load(&name, 16).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("version"), "got: {msg}");
    std::fs::remove_dir_all(&home).ok();
}
