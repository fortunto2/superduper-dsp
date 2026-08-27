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
//! the forced modes only the selected engine runs; a mode change there warms
//! the incoming engine at zero gain until it has fully settled (its padded
//! latency *plus* one STFT window) before the fade starts.

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
/// below it. Picked as the midpoint of the only gap in the measurements —
/// evidence table on [`epoch_sharpness`].
pub const ROUTE_THRESHOLD: f32 = 0.9;

/// Lowest pitch the PSOLA engine will track, in Hz.
///
/// The engine's own default is 95 Hz, which silently never locks on a bass or
/// a low male voice — an 87 Hz take was the finding that started this. 70 Hz
/// reaches down past D2 (73.4 Hz). It is not free: PSOLA's look-behind is
/// 4·T0_max, so the floor sets the latency (70 Hz → 57 ms at 48 kHz, against
/// 42 ms at 95 Hz). Callers that would rather have the milliseconds than the
/// low notes can pass their own floor to [`PitchEngine::with_floor`].
pub const VOICE_FLOOR_HZ: f32 = 70.0;

/// Crossfade length when the route changes, in milliseconds.
const FADE_MS: f32 = 20.0;
/// How often the router re-measures the material, in samples.
const ANALYSIS_HOP: usize = 256;
/// Consecutive disagreeing measurements needed to actually switch. At 256
/// samples / 48 kHz that is ~32 ms of the router being sure before it acts —
/// the gap between a normal and a breathy voice is the narrowest part of the
/// descriptor's range, and a single reading straddling it must not flap the
/// engine.
const VOTES_TO_SWITCH: i32 = 6;

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
    /// Half the buffer: the ring's real length, a power of two.
    hist_len: usize,
    hist_pos: usize,
    hist_filled: usize,
    since_analysis: usize,

    /// Scratch for the engine that is not writing straight to the output.
    scratch: [Box<[f32]>; 2],

    /// True when PSOLA is the engine being faded *towards*.
    want_psola: bool,
    /// Gain applied to PSOLA; the other engine gets the complement. Moves
    /// towards 1.0 or 0.0 over `fade_len`.
    fade_pos: f32,
    fade_step: f32,
    /// Samples the incoming engine still runs at zero gain before the fade
    /// starts (forced-mode changes only — in Auto both are already warm).
    /// Reporting latency is not enough on its own: the phase vocoder's first
    /// valid sample arrives after its padded delay, but its STFT also needs a
    /// whole analysis window of input behind it before the frames it emits
    /// are worth anything. Warming for only the latency left a measured
    /// 1.9 dB dip in the middle of every mode change.
    warmup: usize,
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

        Self {
            psola,
            pvoc,
            mode: Mode::Auto,
            latency,
            hist: vec![0.0; 2 * hist_len].into_boxed_slice(),
            hist_len,
            hist_pos: 0,
            hist_filled: 0,
            since_analysis: 0,
            scratch: [
                vec![0.0; max_frames.max(1)].into_boxed_slice(),
                vec![0.0; max_frames.max(1)].into_boxed_slice(),
            ],
            // Start on PSOLA: it is the engine with the independent formant
            // axis, so it is what a user gets unless the material argues.
            want_psola: true,
            fade_pos: 1.0,
            fade_step: 1.0 / fade_len,
            warmup: 0,
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

    /// Change mode. In a forced mode the incoming engine is cold, so it is
    /// warmed at zero gain for one latency before the fade begins.
    pub fn set_mode(&mut self, mode: Mode) {
        if mode == self.mode {
            return;
        }
        let was_auto = self.mode == Mode::Auto;
        self.mode = mode;
        let want = match mode {
            Mode::Psola => true,
            Mode::Pvoc => false,
            Mode::Auto => self.want_psola,
        };
        if want != self.want_psola {
            self.want_psola = want;
            self.votes = 0;
            // Coming out of Auto both engines are already warm; coming out of
            // a forced mode the other one has been idle.
            self.warmup = if was_auto { 0 } else { self.latency + crate::pvoc::N };
        }
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
        self.pvoc.reset();
        self.hist.fill(0.0);
        self.hist_pos = 0;
        self.hist_filled = 0;
        self.since_analysis = 0;
        self.votes = 0;
        self.warmup = 0;
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

        if self.mode == Mode::Auto {
            self.route(in_l, in_r, n);
        }

        // Where the fade will be by the end of this block decides whether the
        // second engine has to run at all.
        let start = self.fade_pos;
        let target = if self.want_psola { 1.0 } else { 0.0 };
        let moving = self.warmup > 0 || (target - start).abs() > 1e-6;
        let both = self.mode == Mode::Auto || moving;

        if !both {
            if self.want_psola {
                self.psola.process(in_l, in_r, out_l, out_r, p);
            } else {
                self.pvoc.process(in_l, in_r, out_l, out_r, p);
            }
            return;
        }

        // PSOLA into the output, phase vocoder into scratch, then mix down.
        self.psola.process(in_l, in_r, out_l, out_r, p);
        {
            // Field-disjoint borrows: `pvoc` and `scratch` are separate
            // fields, but only a destructure tells the borrow checker that.
            let Self { pvoc, scratch, .. } = self;
            let (a, b) = scratch.split_at_mut(1);
            pvoc.process(in_l, in_r, &mut a[0][..n], &mut b[0][..n], p);
        }

        for i in 0..n {
            if self.warmup > 0 {
                self.warmup -= 1;
            } else if self.fade_pos < target {
                self.fade_pos = (self.fade_pos + self.fade_step).min(target);
            } else if self.fade_pos > target {
                self.fade_pos = (self.fade_pos - self.fade_step).max(target);
            }
            // Equal-power, and that is a measurement rather than a habit.
            // The right law depends on how correlated the two renderings are,
            // and they turn out to be mostly independent: r = 0.13 on the
            // pulsed source, 0.60 on the smooth one
            // (`how_correlated_are_the_two_engines`). Independent signals sum
            // in power, so a linear law would dip mid-fade; equal-power holds
            // the level (measured excursion 0.9 dB either way, which is the
            // correlated residue).
            let a = (self.fade_pos * core::f32::consts::FRAC_PI_2).sin();
            let b = (self.fade_pos * core::f32::consts::FRAC_PI_2).cos();
            out_l[i] = out_l[i] * a + self.scratch[0][i] * b;
            if i < out_r.len() {
                out_r[i] = out_r[i] * a + self.scratch[1][i] * b;
            }
        }
    }

    /// Measure the material and update the routing vote. Cheap: one
    /// [`epoch_sharpness`] call per [`ANALYSIS_HOP`] samples.
    fn route(&mut self, in_l: &[f32], in_r: &[f32], n: usize) {
        let l = self.hist_len;
        for (i, &xl) in in_l[..n].iter().enumerate() {
            let m = 0.5 * (xl + *in_r.get(i).unwrap_or(&xl));
            self.hist[self.hist_pos] = m;
            self.hist[self.hist_pos + l] = m;
            self.hist_pos = (self.hist_pos + 1) & (l - 1);
            self.hist_filled = (self.hist_filled + 1).min(l);
            self.since_analysis += 1;
        }
        if self.since_analysis < ANALYSIS_HOP {
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

        let argues_psola = s >= ROUTE_THRESHOLD;
        if argues_psola == self.want_psola {
            self.votes = 0;
            return;
        }
        self.votes += 1;
        if self.votes >= VOTES_TO_SWITCH {
            self.votes = 0;
            self.want_psola = argues_psola;
        }
    }
}
