//! SuperDuper Tune — autotune / pitch-correction engine.
//!
//! Reuses the shared TD-PSOLA shifter (`superduper_synth_core::psola`) and YIN
//! tracker (`superduper_synth_core::pitch`). The engine measures the singer's
//! pitch, decides a **target** note, and drives the shifter with the semitone
//! correction. Three target sources:
//!
//! - **Scale** — snap to the nearest note in a key/scale (classic autotune;
//!   `Retune = 0` → hard T-Pain snap, courtesy of the shifter's built-in ~5 ms
//!   pitch smoothing).
//! - **MIDI** — pull the voice to a played MIDI note (Auto-Tune "graph"/MIDI).
//! - **Sidechain** — follow the pitch of a reference audio input (sing to a
//!   synth line).
//!
//! Formant is shifted independently (or left transparent) by the PSOLA engine,
//! so correction doesn't chipmunk the timbre. RT-safe: all buffers pre-allocated
//! in [`Tune::new`]; `process` never allocates.

use superduper_synth_core::pitch::YinPitchTracker;
pub use superduper_synth_core::pitch_engine::Mode as EngineMode;
use superduper_synth_core::pitch_engine::PitchEngine;
use superduper_synth_core::psola::PitchParams;
use superduper_synth_core::swiftf0::SwiftF0Tracker;
use crate::scale;

pub const TARGET_SCALE: u32 = 0;
pub const TARGET_MIDI: u32 = 1;
pub const TARGET_SIDECHAIN: u32 = 2;

/// Lowest / highest tracked fundamental for the correction decision (vocal
/// range, a bit wider). Kept within the shifter's own tracked range.
const MIN_HZ: f32 = 70.0;
const MAX_HZ: f32 = 1000.0;

/// Below this the tracker is treated as unvoiced → correction is frozen (no
/// snapping silence / breath to a random note).
const VOICED_HZ: f32 = 55.0;

/// Hard clamp on how far the correction may pull, so an octave-error in the
/// tracker can't throw the voice a huge interval.
const MAX_CORRECTION_ST: f32 = 12.0;

/// SwiftF0's confidence below which a frame counts as unvoiced.
const SWIFT_CONF_GATE: f32 = 0.5;

#[derive(Clone, Copy)]
pub struct TuneParams {
    /// Key root, 0..11 (0 = C). Used by Scale target.
    pub key: u8,
    /// 12-bit scale degree mask (see `scale::SCALES`). Used by Scale target.
    pub scale_mask: u16,
    /// `TARGET_SCALE` / `TARGET_MIDI` / `TARGET_SIDECHAIN`.
    pub target: u32,
    /// Retune time in ms — how fast the correction glides to the target.
    /// 0 = instant (hard tune / T-Pain). Larger = natural, gliding.
    pub retune_ms: f32,
    /// Correction depth, 0..1. 0 = passthrough, 1 = full snap.
    pub amount: f32,
    /// Independent formant shift, semitones.
    pub formant_st: f32,
    /// Dry/Wet, 0..1.
    pub mix: f32,
    /// Output trim, linear gain.
    pub output_lin: f32,
    /// Held MIDI note for `TARGET_MIDI`, or -1 when none is held.
    pub midi_note: i16,
    /// Detect pitch with the SwiftF0 CNN instead of YIN.
    pub use_swiftf0: bool,
    /// Which shifting engine applies the correction. Auto measures the
    /// material — PSOLA on a voice with real glottal pulses, the phase
    /// vocoder on anything smooth or breathy, where PSOLA turns a −66.9 dB
    /// noise floor into −2.6 dB.
    pub engine: EngineMode,
    pub bypassed: bool,
}

impl Default for TuneParams {
    fn default() -> Self {
        Self {
            key: 0,
            scale_mask: scale::SCALES[1].1, // Major
            target: TARGET_SCALE,
            retune_ms: 0.0,
            amount: 1.0,
            formant_st: 0.0,
            mix: 1.0,
            output_lin: 1.0,
            midi_note: -1,
            use_swiftf0: false,
            engine: EngineMode::Auto,
            bypassed: false,
        }
    }
}

pub struct Tune {
    sr: f32,
    /// Tracks the singer (main input).
    in_tracker: YinPitchTracker,
    /// Tracks the sidechain reference (only advanced in Sidechain mode).
    sc_tracker: YinPitchTracker,
    /// The neural alternative to `in_tracker`, selected by `Model`. Only the
    /// SELECTED detector is fed (see `process`) — feeding both would burn a
    /// tracker's worth of CPU for an estimate nothing reads, so switching
    /// mid-phrase costs one window of settling, which the shifter's smoothing
    /// absorbs. Costs ~5% of a core (measured by tools/pitch-bench) — worth it
    /// where YIN flips octaves.
    swift_tracker: SwiftF0Tracker,
    swift_sc_tracker: SwiftF0Tracker,
    /// Both shifting engines plus the router that picks between them.
    engine: PitchEngine,
    /// Retune-smoothed correction in semitones (one-pole toward the target).
    smoothed_corr: f32,
    /// Most recent detected input pitch (Hz), for the GUI.
    last_in_hz: f32,
    /// Most recent applied correction (semitones), for the GUI meter.
    last_corr_st: f32,
}

/// Read the selected detector's estimate.
///
/// SwiftF0 reports its own confidence; below the gate the frame is reported as
/// unvoiced (0 Hz) so the `VOICED_HZ` branch freezes the correction, exactly
/// as YIN's hold-last behaviour does. YIN has no confidence output, so its
/// estimate is taken as-is.
fn selected_hz(swift: &SwiftF0Tracker, yin: &YinPitchTracker, use_swift: bool) -> f32 {
    if !use_swift {
        return yin.current_hz();
    }
    if swift.confidence() >= SWIFT_CONF_GATE {
        swift.current_hz()
    } else {
        0.0
    }
}

fn new_tracker(sr: f32) -> YinPitchTracker {
    YinPitchTracker::new(sr, MIN_HZ, MAX_HZ, 1536, 256, 150.0)
}

impl Tune {
    pub fn new(sr: f32, max_frames: usize) -> Self {
        Self {
            sr,
            in_tracker: new_tracker(sr),
            sc_tracker: new_tracker(sr),
            swift_tracker: SwiftF0Tracker::new(sr, 150.0),
            swift_sc_tracker: SwiftF0Tracker::new(sr, 150.0),
            engine: PitchEngine::new(sr, max_frames),
            smoothed_corr: 0.0,
            last_in_hz: 0.0,
            last_corr_st: 0.0,
        }
    }

    /// Latency reported to the host. Fixed at the max of both engines, so it
    /// does not move when the router switches.
    pub fn latency_samples(&self) -> u32 {
        self.engine.latency_samples()
    }

    pub fn prime(&mut self, mix: f32, output_lin: f32) {
        self.engine.prime(mix, output_lin);
    }

    /// Last epoch-sharpness reading and the current PSOLA share, for the GUI.
    pub fn engine_readout(&self) -> (f32, f32) {
        (self.engine.sharpness(), self.engine.psola_mix())
    }

    pub fn detected_hz(&self) -> f32 {
        self.last_in_hz
    }

    pub fn correction_st(&self) -> f32 {
        self.last_corr_st
    }

    /// Correct one stereo block. `sc_*` is the sidechain reference (pass the main
    /// input again if nothing is routed — Sidechain mode then just tracks the
    /// voice, i.e. no correction).
    pub fn process(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        sc_l: &[f32],
        sc_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
        p: &TuneParams,
    ) {
        let n = in_l.len().min(out_l.len());
        if p.bypassed {
            out_l[..n].copy_from_slice(&in_l[..n]);
            let rn = n.min(in_r.len()).min(out_r.len());
            out_r[..rn].copy_from_slice(&in_r[..rn]);
            return;
        }

        // 1. Feed the trackers so we know the singer's (and reference's) pitch.
        //    Only the selected detector runs — the other would cost CPU for an
        //    estimate nothing reads. A switch therefore costs one window of
        //    settling, which is what the shifter's smoothing already absorbs.
        for i in 0..n {
            let m = (in_l[i] + *in_r.get(i).unwrap_or(&in_l[i])) * 0.5;
            if p.use_swiftf0 {
                self.swift_tracker.push(m);
            } else {
                self.in_tracker.push(m);
            }
            if p.target == TARGET_SIDECHAIN {
                let s = (*sc_l.get(i).unwrap_or(&0.0) + *sc_r.get(i).unwrap_or(&0.0)) * 0.5;
                if p.use_swiftf0 {
                    self.swift_sc_tracker.push(s);
                } else {
                    self.sc_tracker.push(s);
                }
            }
        }
        let f0 = selected_hz(&self.swift_tracker, &self.in_tracker, p.use_swiftf0);
        self.last_in_hz = f0;

        // 2. Decide the target correction (semitones) for this block.
        let raw_corr = if f0 < VOICED_HZ {
            // Unvoiced / silent — hold the current correction (no snapping noise).
            self.smoothed_corr
        } else {
            match p.target {
                TARGET_MIDI => {
                    if p.midi_note >= 0 {
                        scale::correction_to_hz(f0, scale::midi_to_hz(p.midi_note as f32))
                    } else {
                        0.0
                    }
                }
                TARGET_SIDECHAIN => {
                    let scf =
                        selected_hz(&self.swift_sc_tracker, &self.sc_tracker, p.use_swiftf0);
                    if scf < VOICED_HZ { self.smoothed_corr } else { scale::correction_to_hz(f0, scf) }
                }
                _ => scale::nearest_correction_st(f0, p.key, p.scale_mask),
            }
        };
        let target_corr = (raw_corr.clamp(-MAX_CORRECTION_ST, MAX_CORRECTION_ST)) * p.amount.clamp(0.0, 1.0);

        // 3. Retune glide toward the target (one-pole at block rate; 0 ms = jump).
        //    The shifter's own ~5 ms pitch smoothing turns a jump into the classic
        //    hard-tune snap rather than a click.
        let retune_s = (p.retune_ms * 1e-3).max(0.0);
        let coef = if retune_s <= 1e-5 {
            1.0
        } else {
            1.0 - (-(n as f32) / (retune_s * self.sr)).exp()
        };
        self.smoothed_corr += (target_corr - self.smoothed_corr) * coef;
        if !self.smoothed_corr.is_finite() {
            self.smoothed_corr = 0.0;
        }
        self.last_corr_st = self.smoothed_corr;

        // 4. Apply via the shared PSOLA shifter.
        let pp = PitchParams {
            pitch_st: self.smoothed_corr,
            formant_st: p.formant_st,
            mix: p.mix,
            output_lin: p.output_lin,
            bypassed: false,
        };
        self.engine.set_mode(p.engine);
        self.engine.process(in_l, in_r, out_l, out_r, &pp);
    }
}
