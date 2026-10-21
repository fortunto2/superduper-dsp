use crate::{PARAMS, P_DISTANCE, P_LAYOUT, P_LFE, P_MONITOR, P_SPREAD, P_WIDTH, P_X, P_Y, P_Z};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Init (front, 7.1)", &[]),

    Preset::from_overrides("Front left", &[(P_X, -0.6), (P_Y, 0.8)]),
    Preset::from_overrides("Front right", &[(P_X, 0.6), (P_Y, 0.8)]),
    Preset::from_overrides("Behind", &[(P_X, 0.0), (P_Y, -1.0), (P_SPREAD, 0.2)]),
    Preset::from_overrides("Far corner", &[(P_X, 0.9), (P_Y, -0.7), (P_DISTANCE, 0.8)]),
    Preset::from_overrides("Overhead (7.1.4)", &[(P_LAYOUT, 4.0), (P_Z, 1.0), (P_SPREAD, 0.3)]),
    Preset::from_overrides("Room wash", &[(P_SPREAD, 1.0), (P_DISTANCE, 0.5), (P_WIDTH, 2.0)]),
    Preset::from_overrides("Sub to LFE", &[(P_LFE, -6.0), (P_WIDTH, 0.0)]),
    Preset::from_overrides("Stereo pan (monitor)", &[(P_LAYOUT, 0.0), (P_MONITOR, 1.0)]),
];

/// Recall a preset: every value written, every param marked dirty so the
/// host and an agent reading params see the move.
pub fn apply(shared: &crate::SharedParamsInner, preset: &Preset) {
    use std::sync::atomic::Ordering;
    for (i, v) in preset.values.iter().enumerate() {
        if let Some(a) = shared.params.get(i) {
            a.store(*v, Ordering::Relaxed);
            shared.dirty_params[i].store(true, Ordering::Relaxed);
        }
    }
}
