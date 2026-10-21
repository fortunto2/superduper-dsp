//! Morph's copy of the Kubyz factory tables, and its saved state.

use std::sync::atomic::Ordering;
use superduper_morph::targets::{kubyz_model, KUBYZ_COUNT, KUBYZ_NAMES};
use superduper_morph::{PluginShared, PARAMS, P_MORPH, P_SOURCE};
use superduper_synth_core::timbre::{TimbreProfile, GRID, MAX_HARM};

#[test]
fn kubyz_tables_match_the_kubyz_plugin() {
    let src = superduper_kubyz::presets::presets();
    assert_eq!(src.len(), KUBYZ_COUNT);
    for (i, p) in src.iter().enumerate() {
        let m = kubyz_model(i);
        assert_eq!(p.name, KUBYZ_NAMES[i]);
        for k in 0..16 {
            assert!((p.harmonics[k] - m.harmonics[k]).abs() < 1e-6, "{} h{}", p.name, k + 1);
        }
        assert_eq!(m.bands.count, 3);
        assert_eq!(&m.bands.f[..3], &p.formant.f[..], "{}", p.name);
        assert_eq!(&m.bands.bw[..3], &p.formant.bw[..], "{}", p.name);
        assert_eq!(&m.bands.gain[..3], &p.formant.gain[..], "{}", p.name);
        assert_eq!(m.vox_mix, p.default_vox_mix, "{}", p.name);
    }
}

#[test]
fn state_round_trips_params_model_and_capture() {
    let a = PluginShared::new();
    a.write_param(P_MORPH, 0.37);
    a.write_param(P_SOURCE, 1.0);
    let mut model = kubyz_model(3);
    model.harmonics[4] = 0.123;
    model.vox_mix = 0.42;
    a.model.store(&model);
    let mut cap = TimbreProfile { f0: 329.6, harm_valid: true, env_valid: true, noise: 0.07, ..Default::default() };
    for k in 0..MAX_HARM {
        cap.harm_db[k] = -(k as f32) * 1.5;
    }
    for g in 0..GRID {
        cap.env_db[g] = -30.0 + (g as f32 * 0.3).sin() * 10.0;
    }
    a.captured.store(&cap);
    let json = a.save_state_json();

    let b = PluginShared::new();
    b.load_state_json(&json).expect("loads");
    for i in 0..PARAMS.len() {
        assert_eq!(a.params[i].load(Ordering::Relaxed), b.params[i].load(Ordering::Relaxed));
    }
    assert_eq!(b.model.load(), a.model.load());
    assert_eq!(b.captured.load(), cap);
    assert!(b.load_state_json("{nonsense").is_err());
}
