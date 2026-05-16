//! Supermassive-style cascade reverb ported from rust-synth's `preset.rs`.
//!
//! Topology:
//!
//! ```text
//!   stereo → reverb_stereo(35 m, 15 s, 0.88)
//!          → (chorus(3, 0.022, 0.28) | chorus(4, 0.026, 0.28))
//!          → reverb_stereo(50 m, 28 s, 0.90)
//! ```
//!
//! The cascade produces a long shimmering tail with two reverb generations
//! and a stereo chorus between them — Valhalla-Supermassive feel. Sized for
//! ambient pads / cinematic textures, not for short drum-room verbs.
//!
//! `build_wet()` returns a stereo-in / stereo-out `Net` that emits 100% wet
//! signal. The CLAP plugin is in charge of mix/width/dry combine outside
//! this Net — that lets us animate those at sample rate without rebuilding
//! the graph (which would allocate).

use fundsp::prelude::*;

/// Build the pure-wet stereo reverb graph. Caller is responsible for:
/// - calling `Net::set_sample_rate()` after creating it
/// - mixing the wet output with the dry input downstream
pub fn build_wet() -> Net {
    let stage1 = reverb_stereo(35.0, 15.0, 0.88);
    let stage2 = chorus(3, 0.0, 0.022, 0.28) | chorus(4, 0.0, 0.026, 0.28);
    // 2nd reverb damping bumped 0.72 → 0.90 in the original — without it a
    // 28-second T60 accumulates endless 4–8 kHz resonances in the tail.
    let stage3 = reverb_stereo(50.0, 28.0, 0.90);
    Net::wrap(Box::new(stage1 >> stage2 >> stage3))
}
