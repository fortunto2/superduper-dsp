//! SuperDuper Pitch — manual pitch shifter with **independent formant** control.
//!
//! Shift the voice up ("Masyanya" / chipmunk) or down (bass / demon), and move
//! the formants separately for a "manual auto-tune" throat control. Pure and
//! CLAP-free so `tests/` can drive it directly.
//!
//! ## Algorithm — TD-PSOLA (Time-Domain Pitch-Synchronous Overlap-Add)
//!
//! 1. A streaming **YIN** tracker estimates the local pitch period `T0` of the
//!    voice (on the L+R mono sum).
//! 2. Pitch-synchronous **analysis grains** — a Hann window ~2·T0 wide is
//!    conceptually centred on marks spaced `T0` apart.
//! 3. **Synthesis** places grains on marks spaced `T0/α` where `α = 2^(Pitch/12)`.
//!    Denser marks (α>1) raise the pitch; sparser (α<1) lower it. The grain
//!    *content* is unchanged, so the spectral envelope (formants) rides along —
//!    plain PSOLA pitch-shift already preserves formants.
//! 4. **Formant** shift `β = 2^(Formant/12)` is applied by reading each grain's
//!    samples at a `β`-scaled step from the input: the grain's spectral envelope
//!    stretches by `β` **without** touching the synthesis mark spacing, so pitch
//!    and formant move independently. `Formant=0` ⇒ transparent tone; `Pitch=0,
//!    Formant≠0` ⇒ gender/character change with pitch preserved.
//! 5. Grains are overlap-added into an accumulator and normalised by the summed
//!    window (weighted OLA), then blended with the (latency-matched) dry signal.
//!
//! Unvoiced input (YIN unsure) reuses the last period — grains still OLA the
//! content back, so consonants pass through as broadband texture rather than
//! collapsing.
//!
//! Stereo: L/R share the analysis marks + period (from the mono sum) so they
//! stay phase-coherent, but each channel keeps its own grain buffers → the
//! stereo image is preserved.
//!
//! **Latency:** PSOLA needs a few periods of look-behind/ahead. We report it to
//! the host via the CLAP latency extension so PDC keeps parallel tracks aligned.
//! RT-safe: every buffer is pre-allocated in [`PitchShifter::new`]; `process`
//! never allocates.

use crate::dsp_blocks::SmoothedParam;
use crate::pitch::YinPitchTracker;

/// Lowest tracked fundamental. Sets the max period and therefore the latency
/// (kept so 3·T0_max ≤ the shared engine latency).
const MIN_HZ: f32 = 95.0;
/// Highest tracked fundamental.
const MAX_HZ: f32 = 1000.0;

#[derive(Clone, Copy)]
pub struct PitchParams {
    /// Pitch shift in semitones (−24..+24).
    pub pitch_st: f32,
    /// Formant shift in semitones (−12..+12), independent of pitch.
    pub formant_st: f32,
    /// Dry/Wet, 0..1.
    pub mix: f32,
    /// Output trim, linear gain.
    pub output_lin: f32,
    pub bypassed: bool,
}

pub struct PitchShifter {
    sr: f32,
    mask: usize,
    /// Input history, per channel.
    in_ring: [Box<[f32]>; 2],
    /// Overlap-add accumulator, per channel.
    out_ring: [Box<[f32]>; 2],
    /// Summed synthesis window (shared — same window both channels).
    win_ring: Box<[f32]>,
    tracker: YinPitchTracker,
    t0_min: f32,
    t0_max: f32,
    /// How far behind `write_pos` a grain may be finalised (input availability).
    guard: f64,
    /// Reported/processing latency in samples.
    latency: usize,
    write_pos: usize,
    next_synth: f64,
    next_analysis: f64,
    cur_t0: f32,
    sm_pitch: SmoothedParam,
    sm_formant: SmoothedParam,
    sm_mix: SmoothedParam,
    sm_out: SmoothedParam,
}

impl PitchShifter {
    pub fn new(sr: f32, max_frames: usize) -> Self {
        Self::with_latency(sr, max_frames, 0)
    }

    /// Natural look-behind (samples): 4·T0_max covers the 2·T0 epoch grains'
    /// read span (±T0·β), the ±T0/2 epoch search, and the ±T0 output span.
    pub fn natural_latency(sr: f32) -> usize {
        4 * (sr / MIN_HZ).ceil() as usize
    }

    /// `min_latency` lets the caller align this engine's reported latency with a
    /// sibling engine (the phase vocoder) so switching Mode never changes the
    /// host-reported latency. The natural PSOLA latency (3·T0_max) is used if
    /// it's larger.
    pub fn with_latency(sr: f32, max_frames: usize, min_latency: usize) -> Self {
        let t0_max = (sr / MIN_HZ).ceil();
        let t0_min = (sr / MAX_HZ).floor().max(4.0);
        let t0_max_i = t0_max as usize;
        // Same formula as `natural_latency` (single source of truth so the
        // reported PDC and the intrinsic look-behind can't drift apart).
        let latency = Self::natural_latency(sr).max(min_latency);
        let ring = (latency + max_frames + 4 * t0_max_i + 8).next_power_of_two();
        let default_t0 = sr / 150.0;
        Self {
            sr,
            mask: ring - 1,
            in_ring: [vec![0.0; ring].into_boxed_slice(), vec![0.0; ring].into_boxed_slice()],
            out_ring: [vec![0.0; ring].into_boxed_slice(), vec![0.0; ring].into_boxed_slice()],
            win_ring: vec![0.0; ring].into_boxed_slice(),
            tracker: YinPitchTracker::new(sr, MIN_HZ, MAX_HZ, 1536, 256, 150.0),
            t0_min,
            t0_max,
            guard: 3.0 * t0_max as f64,
            latency,
            write_pos: 0,
            next_synth: 0.0,
            next_analysis: 0.0,
            cur_t0: default_t0.clamp(t0_min, t0_max),
            sm_pitch: SmoothedParam::new(0.0),
            sm_formant: SmoothedParam::new(0.0),
            sm_mix: SmoothedParam::new(1.0),
            sm_out: SmoothedParam::new(1.0),
        }
    }

    /// Latency to report to the host (samples).
    pub fn latency_samples(&self) -> u32 {
        self.latency as u32
    }

    pub fn prime(&mut self, mix: f32, output_lin: f32) {
        self.sm_mix.snap(mix);
        self.sm_out.snap(output_lin);
    }

    /// Snap `nominal` to the nearest energy peak (glottal epoch) within ±T0/2,
    /// summed across channels. Alloc-free scan of the input ring.
    #[inline]
    fn refine_epoch(&self, nominal: f64, t0: f32) -> f64 {
        let search = (t0 * 0.5) as i64;
        if search < 1 {
            return nominal;
        }
        let c0 = nominal.round() as i64;
        let wp = self.write_pos as i64;
        let oldest = wp - self.mask as i64;
        let mut best = c0;
        let mut best_e = -1.0f32;
        let mut off = -search;
        while off <= search {
            let idx = (c0 + off).clamp(oldest, wp);
            let a = (idx as usize) & self.mask;
            let e = self.in_ring[0][a].abs() + self.in_ring[1][a].abs();
            if e > best_e {
                best_e = e;
                best = idx;
            }
            off += 1;
        }
        best as f64
    }

    /// Overlap-add one grain into both channels' accumulators.
    ///
    /// The grain is **2·T0** wide (Hann) and centred on a detected epoch (energy
    /// peak) so its energy is concentrated at the glottal pulse and the window
    /// tapers the neighbouring pulses to ~0. That makes it both smooth
    /// (≥50 % overlap around unity) — no click — AND correctly pitch-shiftable:
    /// on downshift the marks spread out and skip pulses (lowering the pitch),
    /// on upshift they pack in and repeat pulses (raising it). Weighted OLA
    /// (normalised by the summed window) keeps the gain flat.
    #[inline]
    fn place_grain(&mut self, center: f64, synth_pos: f64, t0: f32, beta: f32) {
        let ti = t0.round().max(2.0);
        let l = (2.0 * ti) as usize;
        let mask = self.mask;
        let wp = self.write_pos as i64;
        let oldest = wp - mask as i64;
        let inv_l = core::f32::consts::TAU / l as f32;
        for k in 0..l {
            let kf = k as f32;
            let win = 0.5 - 0.5 * (inv_l * kf).cos();
            // Input read position — formant scales the offset from the centre.
            let in_pos = center + ((kf - ti) * beta) as f64;
            let ip = in_pos.floor();
            let frac = (in_pos - ip) as f32;
            let i0 = (ip as i64).clamp(oldest, wp);
            let i1 = (i0 + 1).clamp(oldest, wp);
            let a0 = (i0 as usize) & mask;
            let a1 = (i1 as usize) & mask;
            // Output position (grain centred at the synthesis mark).
            let oi = (synth_pos + (kf - ti) as f64).round() as i64;
            if oi < 0 {
                continue;
            }
            let om = (oi as usize) & mask;
            let sl = self.in_ring[0][a0] + (self.in_ring[0][a1] - self.in_ring[0][a0]) * frac;
            let sr = self.in_ring[1][a0] + (self.in_ring[1][a1] - self.in_ring[1][a0]) * frac;
            self.out_ring[0][om] += sl * win;
            self.out_ring[1][om] += sr * win;
            self.win_ring[om] += win;
        }
    }

    /// Process one stereo block. For mono input pass the same slice for L and R.
    #[allow(clippy::too_many_arguments)]
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
        let sr = self.sr;
        let mask = self.mask;

        for i in 0..n {
            let xl = in_l[i];
            let xr = *in_r.get(i).unwrap_or(&xl);
            let wm = self.write_pos & mask;
            self.in_ring[0][wm] = xl;
            self.in_ring[1][wm] = xr;

            // Track pitch on the mono sum.
            let m = (xl + xr) * 0.5;
            self.tracker.push(m);
            let target_t0 = (sr / self.tracker.current_hz()).clamp(self.t0_min, self.t0_max);
            self.cur_t0 += (target_t0 - self.cur_t0) * 0.002;

            let pitch = self.sm_pitch.step(p.pitch_st, sr);
            let formant = self.sm_formant.step(p.formant_st, sr);
            let alpha = 2f32.powf(pitch / 12.0).max(0.05);
            let beta = 2f32.powf(formant / 12.0);

            // Emit synthesis grains up to the finalisation horizon.
            let horizon = self.write_pos as f64 - self.guard;
            let t0d = self.cur_t0 as f64;
            let mut guard_iters = 0u32;
            while self.next_synth < horizon && guard_iters < 64 {
                while self.next_analysis + t0d <= self.next_synth {
                    self.next_analysis += t0d;
                }
                let nominal = if (self.next_synth - self.next_analysis)
                    > (self.next_analysis + t0d - self.next_synth)
                {
                    self.next_analysis + t0d
                } else {
                    self.next_analysis
                };
                // Snap the grain centre to the nearest energy peak (epoch /
                // glottal pulse) within ±T0/2, so the 2·T0 window lands on a
                // pulse — grain edges fall at low-energy points → no clicks.
                let center = self.refine_epoch(nominal, self.cur_t0);
                self.place_grain(center, self.next_synth, self.cur_t0, beta);
                self.next_synth += t0d / alpha as f64;
                guard_iters += 1;
            }

            // Read the latency-delayed output slot.
            let mix = self.sm_mix.step(p.mix, sr);
            let og = self.sm_out.step(p.output_lin, sr);
            let rp_abs = self.write_pos as i64 - self.latency as i64;
            let (mut ol, mut or) = (0.0f32, 0.0f32);
            if rp_abs >= 0 {
                let rp = (rp_abs as usize) & mask;
                // Clamp the window-sum denominator to the unity-overlap sum
                // (Hann COLA at the α=1 50 % overlap = 1.0) so the normalisation
                // never AMPLIFIES. In low-/zero-overlap regions (downshift, esp.
                // the α≤0.5 degenerate case where 2·T0 grains just touch) the old
                // 1e-6 floor blew tapered grain edges up into spikes → clicks at
                // T0 spacing, which also masked the real (lower) pitch. Capping
                // at 1.0 keeps the taper, killing the seam clicks and letting the
                // downshift actually lower the pitch.
                let w = self.win_ring[rp].max(1.0);
                let wl = self.out_ring[0][rp] / w;
                let wr = self.out_ring[1][rp] / w;
                let dl = self.in_ring[0][rp];
                let dr = self.in_ring[1][rp];
                self.out_ring[0][rp] = 0.0;
                self.out_ring[1][rp] = 0.0;
                self.win_ring[rp] = 0.0;
                ol = (dl * (1.0 - mix) + wl * mix) * og;
                or = (dr * (1.0 - mix) + wr * mix) * og;
            }
            if !ol.is_finite() {
                ol = 0.0;
            }
            if !or.is_finite() {
                or = 0.0;
            }
            out_l[i] = ol;
            if i < out_r.len() {
                out_r[i] = or;
            }

            self.write_pos += 1;
        }
    }
}
