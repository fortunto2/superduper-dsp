//! Pick the pitch engine by material, in one place, so Pitch and Tune behave
//! identically.
//!
//! Neither engine is good at everything, and the difference is not subtle:
//!
//! | material | TD-PSOLA at unity | phase vocoder |
//! |---|---|---|
//! | pulsed voice | −24.2 dB noise-to-harmonic | −23.9 dB |
//! | smooth tone (synth, sustained kubyz) | **−2.6 dB** | −66.9 dB |
//! | breathy take | −1.4 dB | −9.0 dB |
//!
//! PSOLA reads each grain around a snapped glottal epoch and writes it to an
//! unsnapped synthesis mark. When the epoch is real that offset is harmless;
//! when there is no pulse the read point wanders and the grains overlap-add at
//! scrambled phase. It is a scope defect, not an algorithm defect — three
//! attempts to fix the grain scheduler were measured and reverted (lesson 24)
//! — so the fix is to respect the scope: measure the material with
//! [`crate::pitch::epoch_sharpness`] and route.
//!
//! PSOLA is not merely the legacy path, either: it is the one that shifts
//! pitch and formant **independently**, which is what makes "manual
//! auto-tune" and gender-flip work. So [`Mode::Auto`] prefers it and falls
//! back, rather than the other way round.
//!
//! ## Two things that look like details and are not
//!
//! **Latency is fixed at construction, at the max of both engines.** Hosts
//! mis-handle latency that changes at runtime, and PDC that moves mid-song
//! throws a parallel bus out of phase. Both engines are padded up to the same
//! number, which also means their outputs are sample-aligned — a crossfade
//! between them is a crossfade between two versions of the same moment, not
//! between two moments.
//!
//! **In [`Mode::Auto`] both engines run every block.** An engine started cold
//! emits nothing for its whole latency (the phase vocoder: 1536 samples), so
//! fading one in from cold is a fade into silence. Keeping both warm is what
//! makes a route change inaudible, and it is the honest CPU price of that. In
//! the forced modes only the selected engine runs, which is why the fade is
//! gated on one measured quantity — how long both engines have actually been
//! fed — rather than on which mode we came from. Each engine reports its own
//! `settling_samples()`; the router never learns what they are made of.

use crate::dsp_blocks::equal_power;
use crate::pitch::epoch_sharpness;
use crate::psola::{PitchParams, PitchShifter};
use crate::pvoc::PhaseVocoder;

/// Which engine runs.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    /// TD-PSOLA. Monophonic, independent formant control, needs glottal
    /// epochs.
    Psola,
    /// STFT phase vocoder. Handles polyphony and smooth/noisy material;
    /// formant control is an envelope shift rather than a separate axis.
    Pvoc,
    /// Measure the material and pick, with hysteresis.
    #[default]
    Auto,
}

/// Route to PSOLA at or above this epoch sharpness, to the phase vocoder
/// below it. The number belongs to the descriptor that produces it, next to
/// the measurements that justify it — this is a re-export for callers who
/// think in terms of routing.
pub use crate::pitch::EPOCH_SHARPNESS_THRESHOLD as ROUTE_THRESHOLD;

/// Lowest pitch the PSOLA engine will track, in Hz.
///
/// The engine's own default is 95 Hz, which silently never locks on a bass or
/// a low male voice — an 87 Hz take was the finding that started this. 70 Hz
/// reaches down past D2 (73.4 Hz). It is not free: PSOLA's look-behind is
/// 4·T0_max, so the floor sets the latency (70 Hz → 57 ms at 48 kHz, against
/// 42 ms at 95 Hz). Callers that would rather have the milliseconds than the
/// low notes can pass their own floor to [`PitchEngine::with_floor`].
///
/// The 15 ms is not bought for a marginal quality gain: at 95 Hz an 87 Hz
/// voice does not track 8 % flat, it does not track at all — the estimate
/// sits at the tracker's 150 Hz default, 943 cents out, while
/// noise-to-harmonic moves less than 1 dB. An autotune driven by that reads a
/// wrong note and nothing sounds broken. Measured in
/// `the_floor_is_the_difference_between_locking_and_not`.
///
/// Known cost, accepted deliberately: Pitch's Track mode never runs PSOLA and
/// still pays the 15 ms. Making latency depend on the mode is the one thing
/// hosts mishandle, so it stays uniform.
pub const VOICE_FLOOR_HZ: f32 = 70.0;

/// Crossfade length when the route changes, in milliseconds.
const FADE_MS: f32 = 20.0;
/// How often the router re-measures the material, in milliseconds.
const ANALYSIS_MS: f32 = 5.33;
/// How long the descriptor must keep disagreeing with the current route
/// before the engine actually changes. The gap between a normal and a breathy
/// voice is the narrowest part of the descriptor's range, so a single reading
/// straddling it must not flap the engine.
///
/// In milliseconds, not in analyses: expressed as a count it would mean 32 ms
/// at 48 kHz and 8 ms at 192 kHz, i.e. the hysteresis policy would quietly
/// change with the host's sample rate.
const HOLD_MS: f32 = 32.0;

pub struct PitchEngine {
    psola: PitchShifter,
    pvoc: PhaseVocoder,
    mode: Mode,
    latency: usize,

    /// Mono input history the descriptor reads. Held as a **doubled** ring
    /// (every sample written twice, `L` apart) so any trailing window up to
    /// `L` long is one contiguous slice — the descriptor takes a `&[f32]`, and
    /// the alternative to doubling is copying across the seam on every
    /// analysis.
    hist: Box<[f32]>,
    hist_pos: usize,
    hist_filled: usize,
    since_analysis: usize,

    /// Scratch for the engine that is not writing straight to the output.
    /// Two fields rather than an array: index projections do not split a
    /// borrow, field projections do, so an array would need a destructure and
    /// `split_at_mut` just to hand both halves to `pvoc.process`.
    scratch_l: Box<[f32]>,
    scratch_r: Box<[f32]>,

    /// True when PSOLA is the engine being faded *towards*.
    want_psola: bool,
    /// Gain applied to PSOLA; the other engine gets the complement. Moves
    /// towards 1.0 or 0.0 over `fade_len`.
    fade_pos: f32,
    fade_step: f32,
    /// Samples between descriptor readings, and how many agreeing readings
    /// switch the route — both derived from ms at construction so the policy
    /// does not change with sample rate.
    analysis_hop: usize,
    votes_to_switch: i32,
    /// Consecutive samples for which BOTH engines have been fed. The fade may
    /// only advance once this reaches the incoming engine's settling time —
    /// an engine that has just started emits nothing for its whole latency, so
    /// fading into it any earlier is a fade into silence (measured: a 1.9 dB
    /// dip in the middle of every mode change).
    ///
    /// One quantity rather than a flag plus a countdown. The countdown version
    /// armed itself from a table of mode pairs, and the table did not cover
    /// forced Voice → Auto: the target does not move there, so nothing was
    /// armed, and the fade was left relying on the router's own vote delay
    /// happening to exceed the vocoder's latency. Measured, it does — the
    /// route change after that switch shows no dip either way — so this is a
    /// structural guarantee replacing a coincidence, not a bug fix. The dip it
    /// does prevent is the forced Psola → Pvoc one, covered by
    /// `the_crossfade_holds_its_level`.
    warm_samples: usize,
    votes: i32,
    /// Last measured sharpness, for meters and tests.
    sharpness: f32,
}

impl PitchEngine {
    /// `max_frames` is the host's maximum block size. The floor defaults to
    /// [`VOICE_FLOOR_HZ`].
    pub fn new(sr: f32, max_frames: usize) -> Self {
        Self::with_floor(sr, max_frames, VOICE_FLOOR_HZ)
    }

    /// Same, choosing the lowest pitch PSOLA will track. See
    /// [`VOICE_FLOOR_HZ`] for what the choice costs.
    pub fn with_floor(sr: f32, max_frames: usize, min_hz: f32) -> Self {
        let min_hz = min_hz.clamp(20.0, 400.0);
        // Both engines must report — and actually incur — the same latency,
        // or the crossfade would mix two different moments.
        let latency = PitchShifter::natural_latency_for(sr, min_hz).max(crate::pvoc::LATENCY);
        let psola = PitchShifter::with_range(sr, max_frames, latency, min_hz, 1000.0);
        let pvoc = PhaseVocoder::new(sr, latency);
        debug_assert_eq!(psola.latency_samples() as usize, pvoc.latency());

        // The descriptor wants several whole periods of history; T0_max is
        // sr/min_hz, and it reads up to 9 of them.
        let want = (9.0 * sr / min_hz) as usize;
        let hist_len = want.next_power_of_two();
        let fade_len = (FADE_MS * 0.001 * sr).max(1.0);
        let analysis_hop = (ANALYSIS_MS * 0.001 * sr).max(16.0) as usize;
        let votes_to_switch =
            ((HOLD_MS * 0.001 * sr) / analysis_hop as f32).round().max(1.0) as i32;

        Self {
            psola,
            pvoc,
            mode: Mode::Auto,
            latency,
            hist: vec![0.0; 2 * hist_len].into_boxed_slice(),
            hist_pos: 0,
            hist_filled: 0,
            since_analysis: 0,
            analysis_hop,
            votes_to_switch,
            scratch_l: vec![0.0; max_frames.max(1)].into_boxed_slice(),
            scratch_r: vec![0.0; max_frames.max(1)].into_boxed_slice(),
            // Start on PSOLA: it is the engine with the independent formant
            // axis, so it is what a user gets unless the material argues.
            want_psola: true,
            fade_pos: 1.0,
            fade_step: 1.0 / fade_len,
            warm_samples: 0,
            votes: 0,
            sharpness: 0.0,
        }
    }

    /// Latency to report to the host, in samples. Constant for the life of
    /// the engine, whichever way it routes.
    pub fn latency_samples(&self) -> u32 {
        self.latency as u32
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Change mode.
    pub fn set_mode(&mut self, mode: Mode) {
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        let want = match mode {
            Mode::Psola => true,
            Mode::Pvoc => false,
            Mode::Auto => self.want_psola,
        };
        if want != self.want_psola {
            self.want_psola = want;
            self.votes = 0;
        }
    }

    /// The gain the crossfade is heading for.
    #[inline]
    fn target(&self) -> f32 {
        if self.want_psola {
            1.0
        } else {
            0.0
        }
    }

    /// How long the engine being faded *towards* needs before its output is
    /// worth hearing. The outgoing one is running by definition, so only the
    /// incoming one's settling time gates the fade.
    #[inline]
    fn incoming_settling(&self) -> usize {
        if self.want_psola {
            self.psola.settling_samples()
        } else {
            self.pvoc.settling_samples()
        }
    }

    /// The pitch PSOLA is currently tracking, in Hz.
    ///
    /// Exposed so a caller that needs the singer's f0 can read the tracker
    /// already running in here instead of standing up a second identical one.
    pub fn tracked_hz(&self) -> f32 {
        self.psola.tracked_hz()
    }

    /// Last measured epoch sharpness, and which engine that argues for.
    /// For GUI meters and tests — not used by `process`.
    pub fn sharpness(&self) -> f32 {
        self.sharpness
    }

    /// Current PSOLA share of the output, 0..1. 1.0 = pure PSOLA.
    pub fn psola_mix(&self) -> f32 {
        self.fade_pos
    }

    pub fn prime(&mut self, mix: f32, output_lin: f32) {
        self.psola.prime(mix, output_lin);
    }

    pub fn reset(&mut self) {
        self.psola.reset();
        self.pvoc.reset();
        self.hist.fill(0.0);
        self.hist_pos = 0;
        self.hist_filled = 0;
        self.since_analysis = 0;
        self.votes = 0;
        self.warm_samples = 0;
        self.fade_pos = if self.want_psola { 1.0 } else { 0.0 };
    }

    /// Process one stereo block. For mono input pass the same slice for L and
    /// R, exactly like the two engines it wraps.
    pub fn process(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
        p: &PitchParams,
    ) {
        let n = in_l.len().min(out_l.len());
        if p.bypassed {
            out_l[..n].copy_from_slice(&in_l[..n]);
            let rn = n.min(in_r.len()).min(out_r.len());
            out_r[..rn].copy_from_slice(&in_r[..rn]);
            return;
        }
        // A host handing over more frames than it declared at activate would
        // otherwise index past the scratch buffers and panic. Every other
        // stage in the chain degrades instead, so chunk to the size we have.
        let cap = self.scratch_l.len();
        if n > cap {
            let mut at = 0;
            while at < n {
                let k = cap.min(n - at);
                let il = &in_l[at..at + k];
                let ir = in_r.get(at..at + k).unwrap_or(il);
                let (_, out_l_tail) = out_l.split_at_mut(at);
                if out_r.len() >= at + k {
                    let (_, out_r_tail) = out_r.split_at_mut(at);
                    self.process(il, ir, &mut out_l_tail[..k], &mut out_r_tail[..k], p);
                } else {
                    self.process(il, ir, &mut out_l_tail[..k], &mut [], p);
                }
                at += k;
            }
            return;
        }

        // Measured in EVERY mode, not just Auto. The reading does two jobs:
        // it picks the engine (Auto only), and it gates PSOLA's per-grain
        // epoch snap (always). The second matters most exactly where the
        // first is switched off — a user who forces Voice on a synth pad gets
        // +2.7 dB of noise with the snap and −33.4 dB without it.
        self.route(in_l, in_r, n);

        // Where the fade will be by the end of this block decides whether the
        // second engine has to run at all. The `warmup` disjunct is not
        // implied by the fade being off-target: switching away and back inside
        // one warm-up leaves `fade_pos` already AT the target while the idle
        // engine still needs feeding.
        let target = self.target();
        let both = self.mode == Mode::Auto || (target - self.fade_pos).abs() > 1e-6;

        if !both {
            // Only one engine is fed, so the other goes cold and any future
            // fade has to wait for it again.
            self.warm_samples = 0;
            if self.want_psola {
                self.psola.process(in_l, in_r, out_l, out_r, p);
            } else {
                self.pvoc.process(in_l, in_r, out_l, out_r, p);
            }
            return;
        }
        let warm = self.warm_samples >= self.incoming_settling();
        self.warm_samples = self.warm_samples.saturating_add(n);

        // PSOLA into the output, phase vocoder into scratch, then mix down.
        self.psola.process(in_l, in_r, out_l, out_r, p);
        self.pvoc.process(in_l, in_r, &mut self.scratch_l[..n], &mut self.scratch_r[..n], p);

        // Equal-power, and that is a measurement rather than a habit. The
        // right law depends on how correlated the two renderings are, and they
        // turn out to be mostly independent: r = 0.13 on the pulsed source,
        // 0.60 on the smooth one (`how_correlated_are_the_two_engines`).
        // Independent signals sum in power, so a linear law would dip mid-fade;
        // equal-power holds the level (measured excursion 0.9 dB either way,
        // which is the correlated residue).
        //
        // The gains only move while a fade is actually in flight, which is a
        // few blocks in an instance's lifetime — the rest of the time this is
        // one sin/cos per block instead of two per sample. Note it still mixes
        // when parked: at `fade_pos = 1.0` the other engine comes in at
        // cos(π/2) = −4.4e−8, i.e. −147 dB. That is inaudible but not zero, and
        // skipping it would change the rendered output.
        let settled = !warm || self.fade_pos == target;
        let (mut a, mut b) = equal_power(self.fade_pos);
        let n_r = n.min(out_r.len());
        for i in 0..n {
            if !settled {
                if self.fade_pos < target {
                    self.fade_pos = (self.fade_pos + self.fade_step).min(target);
                } else if self.fade_pos > target {
                    self.fade_pos = (self.fade_pos - self.fade_step).max(target);
                }
                (a, b) = equal_power(self.fade_pos);
            }
            out_l[i] = out_l[i] * a + self.scratch_l[i] * b;
            if i < n_r {
                out_r[i] = out_r[i] * a + self.scratch_r[i] * b;
            }
        }
    }

    /// Measure the material and update the routing vote. Cheap: one
    /// [`epoch_sharpness`] call per [`ANALYSIS_MS`].
    fn route(&mut self, in_l: &[f32], in_r: &[f32], n: usize) {
        // The ring is exactly double its usable length, so deriving it here
        // keeps that invariant in one place instead of a field plus a comment.
        let l = self.hist.len() / 2;
        for (i, &xl) in in_l[..n].iter().enumerate() {
            let m = 0.5 * (xl + *in_r.get(i).unwrap_or(&xl));
            self.hist[self.hist_pos] = m;
            self.hist[self.hist_pos + l] = m;
            self.hist_pos = (self.hist_pos + 1) & (l - 1);
        }
        // Both counters are read only after the loop, and clamping is
        // monotone, so once per block gives the same values as once per sample.
        self.hist_filled = (self.hist_filled + n).min(l);
        self.since_analysis += n;
        if self.since_analysis < self.analysis_hop {
            return;
        }
        self.since_analysis = 0;

        // Measure on the period PSOLA is actually cutting grains at, over the
        // trailing history — contiguous thanks to the doubled ring.
        let t0 = self.psola.current_period().round() as usize;
        let need = (9 * t0).min(l);
        if t0 < 8 || need < 3 * t0 || self.hist_filled < need {
            return;
        }
        let end = self.hist_pos + l;
        self.sharpness = epoch_sharpness(&self.hist[end - need..end], t0);
        let s = self.sharpness;

        // Same number, one level down: snap the grain read point only where
        // there is a real epoch to snap to. Measured across sources and
        // shifts, gating lands on the better of always/never every time
        // (`synth-core/tests/epoch_snap.rs`).
        let argues_psola = s >= ROUTE_THRESHOLD;
        self.psola.set_epoch_snap(argues_psola);
        if self.mode != Mode::Auto {
            return;
        }
        if argues_psola == self.want_psola {
            self.votes = 0;
            return;
        }
        self.votes += 1;
        if self.votes >= self.votes_to_switch {
            self.votes = 0;
            self.want_psola = argues_psola;
        }
    }
}
