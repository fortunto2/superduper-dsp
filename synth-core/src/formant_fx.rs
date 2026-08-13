//! Formant articulator — three vocal-tract resonators imposed on whatever is on
//! the input, driven by hand, by a trajectory, or by a live voice.
//!
//! Lives in synth-core (not in the plugin crate) for the same reason
//! `kubyz::voice` does: the mobile staticlib depends only on synth-core, so DSP
//! parked in an `effects/` crate can never reach the phone. SuperDuper Formant
//! re-exports this module as its `dsp`.
//!
//! ## Why this is not the vocoder
//! `superduper-vocoder` copies a modulator's whole
//! spectral envelope onto a carrier — intelligible, and it needs the voice to
//! be sounding at every instant. This plugin models the *mechanism* instead:
//! three band-pass resonances, exactly what a mouth is. That buys three things
//! a vocoder can't give:
//!
//! 1. **Articulation without a voice** — drag the vowel pad, run a trajectory,
//!    or map F1/F2 to a gesture CC. The kubyz "talks" with nobody singing.
//! 2. **A musical result rather than a robotic one** — three resonances glide,
//!    they don't quantise the spectrum into bands.
//! 3. **Continuity across a hand-off** — the voice can stop while the vowel it
//!    was holding stays on the drone (see the tracker's gate/freeze).
//!
//! That last one is the point of the whole plugin: sing a phrase into the
//! sidechain, and the formant path it traced keeps living on a kubyz drone
//! after the voice is gone. Voice → instrument, with one continuous formant
//! line and only the excitation source swapped underneath.
//!
//! ## Signal flow (per sample)
//! ```text
//!   main in ──▶ Drive (tanh) ──▶ [F1 ‖ F2 ‖ F3 band-passes] ──▶ Mix ──▶ Output
//!                                        ▲
//!   voice sidechain ──▶ FormantTracker ──┘   (Follow mode)
//!   MouthShape LFO ──────────────────────┘   (Motion mode)
//! ```
//! **RT-safe:** no allocation, no locks, no syscalls in [`FormantFx::process_stereo`].

use crate::dsp_blocks::tanh_drive;
use crate::formant::Formant;
use crate::formant_track::FormantTracker;
use crate::kubyz::trajectory::MouthShape;

/// Articulation source.
pub const MODE_MANUAL: u32 = 0;
pub const MODE_FOLLOW: u32 = 1;
pub const MODE_MOTION: u32 = 2;

/// Bandwidth as a fraction of centre frequency, per formant. Derived from the
/// Peterson-Barney table (130/730, 180/1090, 260/2440) so `Width = 1` lands on
/// a natural vowel Q instead of an arbitrary one.
const BW_FRAC: [f32; 3] = [0.18, 0.165, 0.11];

/// Motion excursion in Hz at `Depth = 1`. The GUI draws the trajectory with
/// these same constants — keep them in sync.
pub const EX_F1: f32 = 220.0;
pub const EX_F2: f32 = 600.0;

/// Level below which Follow mode freezes the tracked vowel instead of chasing
/// the noise floor.
pub const GATE_DB: f32 = -60.0;

#[derive(Clone, Copy)]
pub struct FmtParams {
    pub f1: f32,
    pub f2: f32,
    pub f3: f32,
    /// Bandwidth scale — < 1 = narrow/vocal, > 1 = broad/airy.
    pub width: f32,
    /// Transposes all three resonances together (semitones).
    pub shift_semi: f32,
    pub mode: u32,
    /// Blend between the manual pad position and the tracked voice (Follow).
    pub follow: f32,
    /// Formant glide time constant (ms).
    pub glide_ms: f32,
    pub path: u32,
    /// Motion rate in Hz — the host-BPM division is resolved by the caller.
    pub rate_hz: f32,
    pub depth: f32,
    /// L/R trajectory phase offset — 1.0 = full anti-phase (wide).
    pub stereo: f32,
    pub drive: f32,
    pub mix: f32,
    pub output_lin: f32,
    pub bypassed: bool,
}

impl Default for FmtParams {
    fn default() -> Self {
        Self {
            f1: 700.0,
            f2: 1200.0,
            f3: 2600.0,
            width: 1.0,
            shift_semi: 0.0,
            mode: MODE_MANUAL,
            follow: 1.0,
            glide_ms: 40.0,
            path: 0,
            rate_hz: 0.5,
            depth: 0.5,
            stereo: 0.0,
            drive: 0.0,
            mix: 1.0,
            output_lin: 1.0,
            bypassed: false,
        }
    }
}

pub struct FormantFx {
    sr: f32,
    /// L channel's resonators — and, when `Stereo = 0`, the single stereo bank
    /// for both channels. Reusing one instance across both paths means toggling
    /// Stereo doesn't hand off between filters holding unrelated state.
    fmt_l: Formant,
    /// R channel's resonators, used only on the split (`Stereo > 0`) path where
    /// the two channels are tuned to different centre frequencies.
    fmt_r: Formant,
    tracker: FormantTracker,
    /// Motion LFO phase in [0, 1).
    phase: f32,
    /// Glided base (pad) formants — kills zipper noise on a pad drag and *is*
    /// the manual articulation smoothing.
    base: [f32; 3],
    primed: bool,
    /// Whether the previous block ran the split (per-channel) path. The two paths
    /// drive different biquad banks, so a switch has to clear the state the other
    /// one left behind.
    was_split: bool,
    /// Last frequencies actually used (published to the GUI).
    cur: [f32; 3],
}

impl FormantFx {
    pub fn new(sr: f32) -> Self {
        Self {
            sr,
            fmt_l: Formant::default(),
            fmt_r: Formant::default(),
            tracker: FormantTracker::new(sr),
            phase: 0.0,
            base: [700.0, 1200.0, 2600.0],
            primed: false,
            was_split: false,
            cur: [700.0, 1200.0, 2600.0],
        }
    }

    /// Snap the glide state to the host-loaded parameter values so the first
    /// block isn't a sweep up from the defaults.
    pub fn prime(&mut self, p: &FmtParams) {
        self.base = [p.f1, p.f2, p.f3];
        self.cur = self.base;
        self.primed = true;
    }

    pub fn reset(&mut self) {
        self.fmt_l = Formant::default();
        self.fmt_r = Formant::default();
        self.tracker.reset();
        self.phase = 0.0;
        self.primed = false;
        self.was_split = false;
    }

    /// Frequencies used on the most recent sample — for the GUI's pad cursor.
    #[inline]
    pub fn current_formants(&self) -> [f32; 3] {
        self.cur
    }
    #[inline]
    pub fn motion_phase(&self) -> f32 {
        self.phase
    }
    #[inline]
    pub fn tracker_level_db(&self) -> f32 {
        self.tracker.level_db()
    }
    #[inline]
    pub fn tracker_active(&self) -> bool {
        self.tracker.is_active()
    }
    #[inline]
    pub fn tracked_formants(&self) -> [f32; 3] {
        self.tracker.formants()
    }

    /// `write_r` may be empty for a mono track — then only L is written.
    #[allow(clippy::too_many_arguments)]
    pub fn process_stereo(
        &mut self,
        read_l: &[f32],
        read_r: &[f32],
        write_l: &mut [f32],
        write_r: &mut [f32],
        sc_l: &[f32],
        sc_r: &[f32],
        p: &FmtParams,
    ) {
        let n = read_l.len().min(write_l.len());
        if !self.primed {
            self.prime(p);
        }
        if p.bypassed {
            write_l[..n].copy_from_slice(&read_l[..n]);
            if !write_r.is_empty() {
                let m = n.min(write_r.len()).min(read_r.len());
                write_r[..m].copy_from_slice(&read_r[..m]);
            }
            return;
        }

        let sr = self.sr;
        // One-pole coefficient for the formant glide, per sample.
        let tau = (p.glide_ms.max(0.5)) * 0.001;
        let glide_a = 1.0 - (-1.0 / (tau * sr)).exp();
        let shift_mult = (p.shift_semi / 12.0).exp2();
        let width = p.width.clamp(0.1, 8.0);
        let mix = p.mix.clamp(0.0, 1.0);
        // Drive maps to a tanh pre-gain; unity at 0 so the knob starts clean.
        let drive_gain = 1.0 + p.drive.clamp(0.0, 1.0) * 23.0;
        let drive_comp = if p.drive > 0.001 { 1.0 / drive_gain.sqrt() } else { 1.0 };
        let split = p.stereo > 0.001 && !write_r.is_empty();
        // The split path only ever drives each Formant's L bank, so while Stereo
        // is up `fmt_l`'s R bank sits frozen at whatever it last held. Dialling
        // Stereo back to 0 would then re-enter that stale integrator state as an
        // audible transient in the right channel only. Clear both banks on the
        // transition instead — it happens once per switch, not per sample.
        if split != self.was_split {
            self.fmt_l = Formant::default();
            self.fmt_r = Formant::default();
            self.was_split = split;
        }
        let shape = MouthShape::from_index(p.path);
        let phase_inc = (p.rate_hz.max(0.0) / sr).min(0.5);
        let depth = p.depth.clamp(0.0, 1.0);
        let follow = p.follow.clamp(0.0, 1.0);
        let target = [p.f1, p.f2, p.f3];
        let nyq = sr * 0.48;
        // Per-band gain is flat here: the vowel table's own gains are baked into
        // the bandwidths, and the pad exposes no per-formant level.
        const GAINS: [f32; 3] = [1.0, 1.0, 1.0];

        for i in 0..n {
            let dry_l = read_l[i];
            let dry_r = if i < read_r.len() { read_r[i] } else { dry_l };

            // ---- Tracker (Follow mode only — don't burn FFTs otherwise) ----
            if p.mode == MODE_FOLLOW {
                let key = if i < sc_l.len() && i < sc_r.len() {
                    (sc_l[i] + sc_r[i]) * 0.5
                } else if i < sc_l.len() {
                    sc_l[i]
                } else {
                    0.0
                };
                self.tracker.push(key, p.glide_ms, GATE_DB);
            }

            // ---- Glide the pad position ----------------------------------
            for k in 0..3 {
                self.base[k] += (target[k] - self.base[k]) * glide_a;
            }

            // ---- Resolve the articulation source -------------------------
            let mut f_l = self.base;
            let mut f_r = self.base;
            match p.mode {
                MODE_FOLLOW => {
                    let t = self.tracker.formants();
                    for k in 0..3 {
                        f_l[k] = self.base[k] + (t[k] - self.base[k]) * follow;
                    }
                    f_r = f_l;
                }
                MODE_MOTION => {
                    // Motion offsets are added AFTER the glide — smoothing them
                    // would cancel the very modulation the user asked for.
                    let (x, y) = shape.point(self.phase);
                    f_l[0] = self.base[0] + y * EX_F1 * depth;
                    f_l[1] = self.base[1] + x * EX_F2 * depth;
                    if split {
                        // Anti-phase trajectory on the right channel = width.
                        let (xr, yr) = shape.point(self.phase + 0.5 * p.stereo);
                        f_r[0] = self.base[0] + yr * EX_F1 * depth;
                        f_r[1] = self.base[1] + xr * EX_F2 * depth;
                    } else {
                        f_r = f_l;
                    }
                    self.phase += phase_inc;
                    if self.phase >= 1.0 {
                        self.phase -= 1.0;
                    }
                }
                _ => {}
            }

            // ---- Transpose + clamp, derive bandwidths --------------------
            // Off the split path f_r mirrors f_l exactly, so deriving it would
            // be three clamps and three multiplies of duplicate work.
            let mut bw_l = [0.0f32; 3];
            let mut bw_r = [0.0f32; 3];
            for k in 0..3 {
                f_l[k] = (f_l[k] * shift_mult).clamp(60.0, nyq);
                bw_l[k] = (f_l[k] * BW_FRAC[k] * width).max(20.0);
                if split {
                    f_r[k] = (f_r[k] * shift_mult).clamp(60.0, nyq);
                    bw_r[k] = (f_r[k] * BW_FRAC[k] * width).max(20.0);
                }
            }
            self.cur = f_l;

            // ---- Drive → resonators → mix -------------------------------
            let (in_l, in_r) = if p.drive > 0.001 {
                (
                    tanh_drive(dry_l, drive_gain) * drive_comp,
                    tanh_drive(dry_r, drive_gain) * drive_comp,
                )
            } else {
                (dry_l, dry_r)
            };

            let (wet_l, wet_r) = if split {
                // Independently tuned channels: one mono bank each — three
                // biquads and three coefficient updates per channel. Feeding the
                // stereo `process` a duplicated pair and discarding half would
                // cost six of each (and each update carries a sin/cos).
                (
                    self.fmt_l.process_mono(in_l, sr, f_l, bw_l, GAINS),
                    self.fmt_r.process_mono(in_r, sr, f_r, bw_r, GAINS),
                )
            } else {
                // Shared tuning — one genuine stereo pass.
                self.fmt_l.process(in_l, in_r, sr, f_l, bw_l, GAINS, 1.0)
            };

            let out_l = (dry_l * (1.0 - mix) + wet_l * mix) * p.output_lin;
            write_l[i] = out_l;
            if !write_r.is_empty() && i < write_r.len() {
                write_r[i] = (dry_r * (1.0 - mix) + wet_r * mix) * p.output_lin;
            }
        }
    }
}
