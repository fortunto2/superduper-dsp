//! Factory presets for SuperDuper Chorus. Curated to cover the lush
//! post-punk / shoegaze territory the plugin's designed for: Joy
//! Division Atmosphere → Cocteau Twins shimmer → Vangelis Blade
//! Runner CS-80 lushness.

use crate::PARAMS;

#[derive(Copy, Clone)]
pub struct Preset {
    pub name: &'static str,
    /// Sparse (param_index, value) overrides; everything else takes the
    /// `ParamDef::default` from the table.
    pub overrides: &'static [(usize, f32)],
}

impl Preset {
    pub fn default_values(&self) -> [f32; PARAMS.len()] {
        let mut out = [0.0_f32; PARAMS.len()];
        for (i, p) in PARAMS.iter().enumerate() {
            out[i] = p.default as f32;
        }
        for &(idx, v) in self.overrides {
            if idx < out.len() {
                out[idx] = v;
            }
        }
        out
    }
}

use crate::{P_DEPTH, P_FEEDBACK, P_MIX, P_RATE, P_SPREAD, P_TIME, P_WIDTH};

pub const PRESETS: &[Preset] = &[
    Preset {
        name: "Init",
        overrides: &[],
    },
    // ----- Bands -----
    Preset {
        name: "Joy Division (Atmosphere)",
        // Slow, deep mod, generous spread, no feedback — like a Small
        // Clone on Sumner's guitar through a Vox amp.
        overrides: &[
            (P_RATE, 0.45),
            (P_DEPTH, 0.72),
            (P_TIME, 14.0),
            (P_SPREAD, 1.0),
            (P_WIDTH, 1.0),
            (P_FEEDBACK, 0.0),
            (P_MIX, 0.55),
        ],
    },
    Preset {
        name: "Cocteau Twins (Shimmer)",
        // Fast wide modulation + feedback for that gauzy, slightly
        // metallic shimmer Robin Guthrie tracked his guitars through.
        overrides: &[
            (P_RATE, 2.8),
            (P_DEPTH, 0.85),
            (P_TIME, 10.0),
            (P_SPREAD, 1.0),
            (P_WIDTH, 1.0),
            (P_FEEDBACK, 0.55),
            (P_MIX, 0.6),
        ],
    },
    Preset {
        name: "Boards of Canada (Lo-Fi Tape)",
        // Wobble plus depth + medium delay → tape wow-and-flutter feel.
        // Slight feedback adds a phaser-ish swirl on top.
        overrides: &[
            (P_RATE, 0.18),
            (P_DEPTH, 0.95),
            (P_TIME, 22.0),
            (P_SPREAD, 0.7),
            (P_WIDTH, 0.85),
            (P_FEEDBACK, 0.25),
            (P_MIX, 0.45),
        ],
    },
    Preset {
        name: "Vangelis (Blade Runner CS-80)",
        // Slow, deep, very wide. Reads as the wash around the CS-80
        // brass pad in Blade Runner end credits.
        overrides: &[
            (P_RATE, 0.35),
            (P_DEPTH, 0.9),
            (P_TIME, 20.0),
            (P_SPREAD, 1.0),
            (P_WIDTH, 1.0),
            (P_FEEDBACK, 0.18),
            (P_MIX, 0.65),
        ],
    },
    Preset {
        name: "My Bloody Valentine (Tremolo Chorus)",
        // Fast deep mod evoking Loveless guitar tones.
        overrides: &[
            (P_RATE, 4.5),
            (P_DEPTH, 0.95),
            (P_TIME, 8.0),
            (P_SPREAD, 1.0),
            (P_WIDTH, 1.0),
            (P_FEEDBACK, 0.35),
            (P_MIX, 0.7),
        ],
    },
    Preset {
        name: "80s Synth Lead",
        // Tight modern stereo chorus on a synth lead.
        overrides: &[
            (P_RATE, 1.2),
            (P_DEPTH, 0.6),
            (P_TIME, 12.0),
            (P_SPREAD, 0.8),
            (P_WIDTH, 1.0),
            (P_FEEDBACK, 0.0),
            (P_MIX, 0.4),
        ],
    },
    Preset {
        name: "Subtle Bus Glue",
        // Almost-invisible widener for a mix bus.
        overrides: &[
            (P_RATE, 0.6),
            (P_DEPTH, 0.25),
            (P_TIME, 9.0),
            (P_SPREAD, 1.0),
            (P_WIDTH, 1.0),
            (P_FEEDBACK, 0.0),
            (P_MIX, 0.25),
        ],
    },
];

pub fn apply(shared: &crate::SharedParamsInner, preset: &Preset) {
    use std::sync::atomic::Ordering;
    let values = preset.default_values();
    for (i, v) in values.iter().enumerate() {
        shared.params[i].store(*v, Ordering::Relaxed);
        shared.dirty_params[i].store(true, Ordering::Relaxed);
    }
}
