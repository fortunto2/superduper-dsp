//! The multiband engine, free of CLAP so tests drive it directly.
//!
//! Two signal paths, on purpose:
//!
//! ```text
//!   detector:  x ─► LR4 tree ─► band b ─► |·| max(L,R) ─► envelope ─┐
//!                                       key level (bus) ─► blend ───┴► curve ─► band dB
//!
//!   audio:     x ─► low shelf @c1 ─► … ─► low shelf @c(n−1) ─► × top gain
//!              (shelf + top gains solved so each band centre gets its band dB)
//! ```
//!
//! **Dynamic phase.** The audio never goes through the crossover. The band
//! gains are applied as a staircase of low shelves, one per crossover, plus
//! a broadband gain for the top band. With crossovers far apart each shelf
//! would simply carry the difference between its two bands; closer than ~2
//! octaves their tails overlap, so `GainSolver` solves for the shelf gains
//! whose summed response lands every band's gain at its centre. When all
//! bands want the same gain there is no shelf at all (output = input × gain,
//! state zeroed — the exact state of a unity filter), so with no band
//! compressing the output is the input bit for bit. A band that compresses
//! adds the phase shift of its shelves, and only while it compresses — what
//! FabFilter calls Dynamic Phase. The shelves are 8th order (48 dB/oct), so
//! a narrow band can be cut deeply; a static LR4 sum would instead put a
//! 360° allpass rotation at every crossover on every sample.
//!
//! The detector does use Linkwitz-Riley 4th order (24 dB/oct, sums flat):
//! each band is judged by what is really in it, and Solo listens to it.

use superduper_synth_core::dsp_blocks::Biquad;

pub const MAX_BANDS: usize = 4;
pub const MAX_XOVERS: usize = MAX_BANDS - 1;
/// Upward expansion (ratio < 1) with Range off would grow without limit.
const UPWARD_CAP_DB: f32 = 12.0;
const KEY_SILENT_DB: f32 = -119.0;
/// A key level above this is a bus fault, not a signal.
const KEY_MAX_DB: f32 = 24.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandSettings {
    pub threshold_db: f32,
    pub ratio: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub knee_db: f32,
    pub makeup_db: f32,
    /// Max |gain change|; 0 = unlimited.
    pub range_db: f32,
    /// 0..1: how much the bus key, not the band's own signal, drives it.
    pub key: f32,
    pub solo: bool,
    pub bypass: bool,
}

impl Default for BandSettings {
    fn default() -> Self {
        Self {
            threshold_db: -20.0, ratio: 2.0, attack_ms: 10.0, release_ms: 120.0, knee_db: 6.0,
            makeup_db: 0.0, range_db: 0.0, key: 0.0, solo: false, bypass: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub bands: usize,
    pub xover_hz: [f32; MAX_XOVERS],
    pub mix: f32,
    pub output_db: f32,
    pub band: [BandSettings; MAX_BANDS],
}

impl Default for Settings {
    fn default() -> Self {
        Self { bands: 3, xover_hz: [150.0, 2500.0, 8000.0], mix: 1.0, output_db: 0.0, band: [BandSettings::default(); MAX_BANDS] }
    }
}

/// Smallest ratio between neighbouring crossovers: half an octave. The GUI
/// drag limits and `Settings::sanitized` both use it. It was ⅓ octave until
/// 2026-10-21; measured with `SolverProbe` over 2000 random band gains held
/// for 8 samples, the 8th-order shelves could not deliver a fifth of the
/// ordinary 0..−12 dB combinations at ⅓ octave × 4 bands (395 of 2000 more
/// than 1 dB off with a shelf pinned at its ±72 dB limit) and 822 of the
/// −24..+6 dB ones at 3 bands. At ½ octave both read 0 of 2000.
pub const MIN_XOVER_RATIO: f32 = core::f32::consts::SQRT_2;

impl Settings {
    /// Bands clamped to 2..=4, crossovers kept ascending with
    /// ≥ `MIN_XOVER_RATIO` (½ octave) between them and inside 20 Hz .. 0.45·sr.
    pub fn sanitized(mut self, sr: f32) -> Self {
        self.bands = self.bands.clamp(2, MAX_BANDS);
        let top = (sr * 0.45).min(20_000.0);
        let mut lo = 20.0f32;
        for c in self.xover_hz.iter_mut() {
            *c = c.clamp(lo, top);
            lo = (*c * MIN_XOVER_RATIO).min(top);
        }
        self
    }
}

/// Frequency range of band `b` (20 Hz and 20 kHz at the outer edges).
pub fn band_range(bands: usize, xover_hz: &[f32; MAX_XOVERS], b: usize) -> (f32, f32) {
    let n = bands.clamp(2, MAX_BANDS);
    let lo = if b == 0 { 20.0 } else { xover_hz[b - 1] };
    let hi = if b + 1 >= n { 20_000.0 } else { xover_hz[b] };
    (lo, hi)
}

/// Static curve: gain change in dB for a detector level. `ratio < 1` makes it
/// upward (louder above threshold). Exactly 0.0 below the knee, so an idle
/// band contributes exactly unity.
#[inline]
pub fn static_gain_db(x_db: f32, threshold_db: f32, ratio: f32, knee_db: f32, range_db: f32) -> f32 {
    let slope = 1.0 / ratio.max(0.1) - 1.0;
    let over = x_db - threshold_db;
    let g = if knee_db > 0.0 && 2.0 * over.abs() <= knee_db {
        let t = over + 0.5 * knee_db;
        slope * t * t / (2.0 * knee_db)
    } else if over > 0.0 {
        slope * over
    } else {
        0.0
    };
    let lim = if range_db > 0.05 { range_db } else if g > 0.0 { UPWARD_CAP_DB } else { f32::INFINITY };
    g.clamp(-lim, lim)
}

/// The key track's level over a band: the loudest point among 9 log-spaced
/// reads of `key_db_at`. Atomics only, audio-thread safe.
pub fn band_key_db(scope: &superduper_synth_core::gui::LiveScope, lo: f32, hi: f32) -> f32 {
    let (l, h) = (lo.max(20.0).ln(), hi.max(lo + 1.0).ln());
    (0..9).map(|i| scope.key_db_at((l + (h - l) * i as f32 / 8.0).exp())).fold(-120.0f32, f32::max)
}

/// The bus key's level over every band, read over the crossovers the engine
/// really uses (sanitised: ascending, ≥ ½ octave apart, inside 20 Hz..0.45·sr),
/// not the raw params — a crossover dragged below its neighbour would
/// otherwise key a band on a range it never processes. Atomics only, once
/// per block.
pub fn key_levels(scope: &superduper_synth_core::gui::LiveScope, s: &Settings, sr: f32) -> [f32; MAX_BANDS] {
    let s = s.sanitized(sr);
    std::array::from_fn(|b| {
        let (lo, hi) = band_range(s.bands, &s.xover_hz, b);
        band_key_db(scope, lo, hi)
    })
}

/// Where band `b`'s gain is defined and measured: the log centre of an inner
/// band, one octave beyond its crossover for the outer two (where the shelf
/// staircase is flat). The gain solver aims the response at these points.
pub fn band_ref_hz(bands: usize, xover_hz: &[f32; MAX_XOVERS], b: usize, sr: f32) -> f32 {
    let n = bands.clamp(2, MAX_BANDS);
    let f = if b == 0 {
        xover_hz[0] * 0.5
    } else if b + 1 >= n {
        xover_hz[n - 2] * 2.0
    } else {
        (xover_hz[b - 1] * xover_hz[b]).sqrt()
    };
    f.min(sr * 0.49)
}

// ---------------------------------------------------------------------------
// Detector crossover: LR4 tree
// ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct Lr4 {
    a: Biquad,
    b: Biquad,
}

impl Lr4 {
    #[inline]
    fn run(&mut self, x: f32) -> f32 { self.b.process(self.a.process(x)) }
}

#[derive(Default, Clone, Copy)]
struct Splitter {
    lp: [Lr4; MAX_XOVERS],
    hp: [Lr4; MAX_XOVERS],
}

impl Splitter {
    fn tune(&mut self, sr: f32, xover: &[f32; MAX_XOVERS]) {
        const Q: f32 = core::f32::consts::FRAC_1_SQRT_2;
        for (i, &f) in xover.iter().enumerate() {
            for bq in [&mut self.lp[i].a, &mut self.lp[i].b] { bq.set_lpf(sr, f, Q); }
            for bq in [&mut self.hp[i].a, &mut self.hp[i].b] { bq.set_hpf(sr, f, Q); }
        }
    }

    /// Low band = LP(c1); the rest = HP(c1), split again at c2, … — the
    /// classic LR4 tree. The sum is flat in magnitude. Every stage runs
    /// whatever the band count, so a stage that comes back into use (Bands
    /// 4→2→4 while playing) has the state of the signal, not a stale one.
    #[inline]
    fn split(&mut self, x: f32, n: usize, out: &mut [f32; MAX_BANDS]) {
        let mut rest = [x; MAX_BANDS];
        let mut low = [0.0f32; MAX_XOVERS];
        for i in 0..MAX_XOVERS {
            low[i] = self.lp[i].run(rest[i]);
            rest[i + 1] = self.hp[i].run(rest[i]);
        }
        out[..n - 1].copy_from_slice(&low[..n - 1]);
        out[n - 1] = rest[n - 1];
    }
}

// ---------------------------------------------------------------------------
// Audio path: 8th-order Butterworth low shelves in f64
// ---------------------------------------------------------------------------

/// Biquad sections per shelf.
const SECTIONS: usize = 4;
/// Pole damping of the four sections of an 8th-order Butterworth:
/// sin((2m−1)·π/16), m = 1..4.
const ZETA: [f64; SECTIONS] = [0.195_090_322_016_128_25, 0.555_570_233_019_602_2, 0.831_469_612_302_545_2, 0.980_785_280_403_230_4];
/// |H(jw)|² of the shelf is (A + w¹⁶)/(1/A + w¹⁶), A its linear DC gain.
const SHELF_POW: i32 = 16;
/// Largest gain the solver gives one shelf. Two adjacent bands may legitimately
/// differ by GR + makeup + upward, so this is wide; a narrow band asking for
/// more than its slope allows saturates here instead of running away.
const SHELF_LIMIT_DB: f64 = 72.0;
/// A solved shelf smaller than this is left out (unity, bit-exact).
const SHELF_SNAP_DB: f64 = 0.005;
/// Band gains closer than this to the last solved ones reuse that solution.
const SOLVE_REUSE_DB: f64 = 1e-3;
/// Newton iterations per sample. A solve still further off than
/// `SOLVE_TOL_DB` after them carries on from where it stopped on the next
/// sample (the gains usually hold after a jump), until it lands or a whole
/// pass stops improving — an unreachable request then costs nothing more.
const SOLVE_ITERS: usize = 16;
const SOLVE_TOL_DB: f64 = 0.25;
/// Applied-vs-requested differences below this are not reported to the meter.
const METER_ERR_DB: f64 = 0.05;
/// An inner band can be pushed away from its neighbours by at most about
/// slope × width (48 dB/oct × octaves) at its centre, and only with shelves
/// that grow without bound on the way there. Requests are held to this
/// fraction of it, so the shelves stay moderate and the solve stays square.
const REACH: f64 = 0.8;
/// Crossfade on a topology change (band count, Solo).
const SWITCH_MS: f32 = 5.0;

/// An 8th-order (48 dB/oct) low shelf: Butterworth poles at radius
/// A^(−1/16), zeros at A^(+1/16) around the crossover, so the DC gain is A,
/// the top is unity and the dB midpoint sits on the crossover. 48 dB/oct, not
/// the detector's 24: a band ⅓ octave wide can only be cut by about
/// slope × width at its centre (8 dB with 4th-order shelves, 16 dB here) —
/// the shelves' tails eat the rest. A 2nd-order shelf leaked 0.3 dB two
/// octaves out at 25 dB of GR.
#[derive(Default, Clone, Copy)]
struct ShelfCoef {
    /// Per section: b0, b1, b2, a1, a2 (normalised).
    sec: [[f64; 5]; SECTIONS],
    /// Gain these coefficients were computed for; NaN = stale.
    gain_db: f64,
}

impl ShelfCoef {
    /// `k` = 1 / tan(π·fc / sr), the bilinear transform prewarped at fc.
    fn set(&mut self, k: f64, gain_db: f64) {
        let rz = 10f64.powf(gain_db / (20.0 * SHELF_POW as f64)); // A^(1/16)
        let rp = 1.0 / rz;
        let k2 = k * k;
        for (m, z) in ZETA.iter().enumerate() {
            // Analog (s² + 2ζ·rz·s + rz²) / (s² + 2ζ·rp·s + rp²), s → k(1−z⁻¹)/(1+z⁻¹).
            let (b1, b0) = (2.0 * z * rz, rz * rz);
            let (a1, a0) = (2.0 * z * rp, rp * rp);
            let inv = 1.0 / (k2 + a1 * k + a0);
            self.sec[m] = [
                (k2 + b1 * k + b0) * inv,
                2.0 * (b0 - k2) * inv,
                (k2 - b1 * k + b0) * inv,
                2.0 * (a0 - k2) * inv,
                (k2 - a1 * k + a0) * inv,
            ];
        }
        self.gain_db = gain_db;
    }
}

#[derive(Default, Clone, Copy)]
struct ShelfState {
    z: [[f64; 2]; SECTIONS],
}

impl ShelfState {
    /// Unity shelf: output = input, and the TDF-II state of a unity filter
    /// is zero — so engaging the shelf later starts from the right place.
    #[inline]
    fn run(&mut self, c: &ShelfCoef, active: bool, x: f64) -> f64 {
        if !active {
            self.z = [[0.0; 2]; SECTIONS];
            return x;
        }
        let mut v = x;
        for (z, [b0, b1, b2, a1, a2]) in self.z.iter_mut().zip(c.sec) {
            let y = b0 * v + z[0];
            z[0] = b1 * v - a1 * y + z[1];
            z[1] = b2 * v - a2 * y;
            v = y;
        }
        v
    }
}

// ---------------------------------------------------------------------------
// Band gains → shelf gains
// ---------------------------------------------------------------------------

/// The shelves' tails overlap, so "shelf = difference of neighbouring bands"
/// only gives each band its gain when crossovers are ~2 octaves apart (band 1
/// asked −12 got −4.6 dB at ⅓ octave). This solves for the shelf gains and
/// the broadband top gain whose summed dB response hits every band's gain at
/// its reference point (`band_ref_hz`): Newton on the exact closed-form shelf
/// response, warm-started from the previous sample, so it is one or two
/// iterations while the gains move and none while they hold. Each step is
/// damped (halved while it makes the fit worse), and a jump (a transient)
/// that needs more than one sample's worth of iterations carries on while
/// the gains hold, see `SOLVE_ITERS`. Before 2026-10-21 four undamped
/// iterations were the lot and a held target reused them as they were: at
/// ⅓ octave × 4 bands, 0..−12 dB band gains, 251 of 2000 random held targets
/// were more than 1 dB off with no shelf at its limit (the worst by 11.8 dB);
/// with damping and continuation that reads 0. What is left over is what
/// the shelves cannot reach at all, see `MIN_XOVER_RATIO`.
#[derive(Clone, Copy)]
struct GainSolver {
    /// Band count the influence table was built for; 0 = stale.
    nb: usize,
    /// u[k][c] = w¹⁶, w = band k's reference over crossover c (prewarped).
    u: [[f64; MAX_XOVERS]; MAX_BANDS],
    /// How far inner band k may sit outside its neighbours (dB), see `REACH`.
    reach: [f64; MAX_BANDS],
    /// Unknowns: shelves 0..nb−1, then the top gain at index nb−1.
    x: [f64; MAX_BANDS],
    /// Band gains of the last solve, and the response it achieved.
    target: [f64; MAX_BANDS],
    applied: [f64; MAX_BANDS],
    fresh: bool,
    /// Largest |target − applied| of the last solve, and whether a whole
    /// pass of iterations on these targets stopped improving it.
    err: f64,
    stuck: bool,
}

impl GainSolver {
    fn new() -> Self {
        Self {
            nb: 0, u: [[0.0; MAX_XOVERS]; MAX_BANDS], reach: [0.0; MAX_BANDS], x: [0.0; MAX_BANDS],
            target: [0.0; MAX_BANDS], applied: [0.0; MAX_BANDS], fresh: false, err: 0.0, stuck: false,
        }
    }

    fn refresh(&mut self, sr: f32, xover: &[f32; MAX_XOVERS], nb: usize) {
        if nb != self.nb { self.x = [0.0; MAX_BANDS]; }
        self.nb = nb;
        let t = |f: f32| (core::f64::consts::PI * f as f64 / sr as f64).tan();
        for k in 1..nb - 1 {
            let oct = (t(xover[k]) / t(xover[k - 1])).log2();
            self.reach[k] = REACH * 3.0 * SHELF_POW as f64 * oct;
        }
        for k in 0..nb {
            let fk = t(band_ref_hz(nb, xover, k, sr));
            for c in 0..nb - 1 {
                self.u[k][c] = (fk / t(xover[c])).powi(SHELF_POW);
            }
        }
        self.fresh = false;
    }

    /// Response at every reference point for the current `x`, and its
    /// Jacobian (row k, column = unknown).
    fn eval(&self, act: &mut [f64; MAX_BANDS], jac: &mut [[f64; MAX_BANDS]; MAX_BANDS]) {
        let nb = self.nb;
        let a: [f64; MAX_XOVERS] = std::array::from_fn(|c| if c + 1 < nb { 10f64.powf(self.x[c] / 20.0) } else { 1.0 });
        for k in 0..nb {
            let mut s = self.x[nb - 1];
            for c in 0..nb - 1 {
                let (ac, u) = (a[c], self.u[k][c]);
                s += 10.0 * ((ac + u) / (1.0 / ac + u)).log10();
                jac[k][c] = 0.5 * (ac / (ac + u) + 1.0 / (1.0 + u * ac));
            }
            jac[k][nb - 1] = 1.0;
            act[k] = s;
        }
    }

    /// Largest |target − response| over the bands for unknowns `x`.
    fn max_err(&self, x: &[f64; MAX_BANDS], t: &[f64; MAX_BANDS]) -> f64 {
        let nb = self.nb;
        let a: [f64; MAX_XOVERS] = std::array::from_fn(|c| if c + 1 < nb { 10f64.powf(x[c] / 20.0) } else { 1.0 });
        let mut worst = 0.0f64;
        for k in 0..nb {
            let mut s = x[nb - 1];
            for c in 0..nb - 1 {
                let (ac, u) = (a[c], self.u[k][c]);
                s += 10.0 * ((ac + u) / (1.0 / ac + u)).log10();
            }
            worst = worst.max((t[k] - s).abs());
        }
        worst
    }

    /// Up to `SOLVE_ITERS` damped Newton iterations on targets `t` from
    /// unknowns `start`. Returns the best iterate seen (unknowns, response,
    /// largest |error|): a step that made things worse is undone.
    fn newton_pass(&mut self, t: &[f64; MAX_BANDS], start: [f64; MAX_BANDS]) -> ([f64; MAX_BANDS], [f64; MAX_BANDS], f64) {
        let nb = self.nb;
        self.x = start;
        let mut act = [0.0; MAX_BANDS];
        let mut jac = [[0.0; MAX_BANDS]; MAX_BANDS];
        let (mut best_x, mut best_act, mut best_err) = (self.x, act, f64::INFINITY);
        for it in 0..SOLVE_ITERS {
            self.eval(&mut act, &mut jac);
            let mut r: [f64; MAX_BANDS] = std::array::from_fn(|k| if k < nb { t[k] - act[k] } else { 0.0 });
            let err = r[..nb].iter().fold(0.0f64, |m, e| m.max(e.abs()));
            if err < best_err { (best_x, best_act, best_err) = (self.x, act, err); }
            if it + 1 == SOLVE_ITERS || err < SOLVE_REUSE_DB { break; }
            if !lin_solve(&mut jac, &mut r, nb) { break; }
            self.step(&r, t, err);
        }
        (best_x, best_act, best_err)
    }

    /// One damped Newton step along `d`: the full step when it improves
    /// the fit, else halved up to three times (far from the solution the
    /// shelves' log response is not what the Jacobian says and a full step
    /// overshoots into a worse place, which is where the undamped solver
    /// went round in circles).
    fn step(&mut self, d: &[f64; MAX_BANDS], t: &[f64; MAX_BANDS], err: f64) {
        let nb = self.nb;
        let base = self.x;
        let mut scale = 1.0;
        for attempt in 0..4 {
            let mut x = base;
            for i in 0..nb { x[i] += (d[i] * scale).clamp(-24.0, 24.0); }
            for v in x[..nb - 1].iter_mut() { *v = v.clamp(-SHELF_LIMIT_DB, SHELF_LIMIT_DB); }
            if attempt == 3 || self.max_err(&x, t) < err {
                self.x = x;
                return;
            }
            scale *= 0.5;
        }
    }

    /// Shelf gains (0 = shelf off) and top gain for these band gains.
    fn solve(&mut self, band_db: &[f32; MAX_BANDS]) -> ([f64; MAX_XOVERS], f64) {
        let nb = self.nb;
        let mut t: [f64; MAX_BANDS] = std::array::from_fn(|b| if b < nb { band_db[b] as f64 } else { 0.0 });
        for b in 1..nb - 1 {
            let (lo, hi) = (t[b - 1].min(t[b + 1]), t[b - 1].max(t[b + 1]));
            t[b] = t[b].clamp(lo - self.reach[b], hi + self.reach[b]);
        }
        if (1..nb).all(|b| t[b] == t[0]) {
            // Flat: no shelf at all — the bit-exact idle path.
            self.x = [0.0; MAX_BANDS];
            self.x[nb - 1] = t[0];
            self.target = t;
            self.applied = t;
            self.fresh = true;
            self.err = 0.0;
            self.stuck = false;
            return ([0.0; MAX_XOVERS], t[0]);
        }
        let near = self.fresh && (0..nb).all(|b| (t[b] - self.target[b]).abs() <= SOLVE_REUSE_DB);
        // Same targets as last sample: done if that solve landed, or if it
        // can go no further. Otherwise keep iterating from where it stopped.
        if !(near && (self.err <= SOLVE_TOL_DB || self.stuck)) {
            let before = if near { self.err } else { f64::INFINITY };
            let warm = self.x;
            let (mut best_x, mut best_act, mut best_err) = self.newton_pass(&t, warm);
            if best_err > SOLVE_TOL_DB {
                // The warm start can sit in the wrong basin: shelves left at
                // ±70 dB by a target the layout could not reach keep Newton
                // from the modest solution a flat start finds (13 of 13 such
                // misses at ½ octave × 4 bands, measured). Try from flat too.
                let mut flat = [0.0; MAX_BANDS];
                flat[nb - 1] = t[..nb].iter().sum::<f64>() / nb as f64;
                let (cx, cact, cerr) = self.newton_pass(&t, flat);
                if cerr < best_err { (best_x, best_act, best_err) = (cx, cact, cerr); }
            }
            self.x = best_x;
            self.applied = best_act;
            self.target = t;
            self.fresh = true;
            self.stuck = best_err > before - SOLVE_REUSE_DB;
            self.err = best_err;
        }
        let shelf = std::array::from_fn(|c| {
            let v = if c + 1 < nb { self.x[c] } else { 0.0 };
            if v.abs() < SHELF_SNAP_DB { 0.0 } else { v }
        });
        (shelf, self.x[nb - 1])
    }
}

/// Test hook: the gain solver on its own, so a suite can measure how close
/// the staircase lands to what the bands ask for at a given layout.
#[doc(hidden)]
pub struct SolverProbe(GainSolver);

impl SolverProbe {
    pub fn new(sr: f32, xover: &[f32; MAX_XOVERS], bands: usize) -> Self {
        let mut g = GainSolver::new();
        g.refresh(sr, xover, bands.clamp(2, MAX_BANDS));
        Self(g)
    }

    /// One solve, warm from the last one, as the engine does per sample.
    /// Returns (target the solver aimed at after the reach clamp, response it
    /// achieved) per band, in dB.
    pub fn solve(&mut self, band_db: &[f32; MAX_BANDS]) -> ([f32; MAX_BANDS], [f32; MAX_BANDS]) {
        let _ = self.0.solve(band_db);
        (self.0.target.map(|v| v as f32), self.0.applied.map(|v| v as f32))
    }

    /// Shelf gains of the last solve (dB), and the limit they are held to.
    pub fn shelves(&self) -> ([f32; MAX_XOVERS], f32) {
        (std::array::from_fn(|c| self.0.x[c] as f32), SHELF_LIMIT_DB as f32)
    }
}

/// Gaussian elimination with partial pivoting on the leading n×n block;
/// the solution replaces `b`. False if singular.
fn lin_solve(a: &mut [[f64; MAX_BANDS]; MAX_BANDS], b: &mut [f64; MAX_BANDS], n: usize) -> bool {
    for col in 0..n {
        let p = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs())).unwrap_or(col);
        if a[p][col].abs() < 1e-12 { return false; }
        a.swap(col, p);
        b.swap(col, p);
        for row in col + 1..n {
            let f = a[row][col] / a[col][col];
            for j in col..n { a[row][j] -= f * a[col][j]; }
            b[row] -= f * b[col];
        }
    }
    for col in (0..n).rev() {
        let mut s = b[col];
        for j in col + 1..n { s -= a[col][j] * b[j]; }
        b[col] = s / a[col][col];
    }
    true
}

// ---------------------------------------------------------------------------
// Smoothing that lands exactly on its target (needed for bit-exact idle)
// ---------------------------------------------------------------------------

#[derive(Default, Clone, Copy)]
struct Glide {
    v: f32,
}

impl Glide {
    #[inline]
    fn step(&mut self, target: f32, coef: f32) -> f32 {
        let d = self.v - target;
        self.v = if d.abs() < 1e-5 { target } else { target + d * coef };
        self.v
    }
}

#[inline]
fn db_to_lin(db: f32) -> f32 {
    if db == 0.0 { 1.0 } else { 10f32.powf(db * 0.05) }
}

#[inline]
fn tc_coef(ms: f32, sr: f32) -> f32 { (-1.0 / (ms.max(0.01) * 0.001 * sr)).exp() }

/// What one block did, per band — for meters, the bus and tests.
#[derive(Clone, Copy, Debug, Default)]
pub struct BlockReport {
    /// Most extreme gain change of the block (≤ 0 compressing, > 0 upward):
    /// what the audio path actually applied at the band's reference point,
    /// makeup excluded — so a request the shelves cannot reach reads as what
    /// it got, not as what was asked.
    pub gr_db: [f32; MAX_BANDS],
    /// Detector level at the end of the block (dBFS, after the key blend).
    pub detector_db: [f32; MAX_BANDS],
}

pub struct Engine {
    sr: f32,
    split_l: Splitter,
    split_r: Splitter,
    tuned_xover: [f32; MAX_XOVERS],
    coef: [ShelfCoef; MAX_XOVERS],
    shelf_l: [ShelfState; MAX_XOVERS],
    shelf_r: [ShelfState; MAX_XOVERS],
    solver: GainSolver,
    /// Shelf + top gains applied on the last sample.
    applied_shelf: [f64; MAX_XOVERS],
    applied_top: f64,
    /// Band-count change: glide from these gains to the solved ones.
    fade_shelf: [f64; MAX_XOVERS],
    fade_top: f64,
    fade_left: u32,
    nb: usize,
    env: [f32; MAX_BANDS],
    key_env: [f32; MAX_BANDS],
    makeup: [Glide; MAX_BANDS],
    /// Solo crossfade: weight of the full (staircase) path, and of each band.
    full_w: Glide,
    solo_w: [Glide; MAX_BANDS],
    mix: Glide,
    output: Glide,
}

impl Engine {
    pub fn new(sr: f32, s: &Settings) -> Self {
        let s = s.sanitized(sr);
        let any_solo = (0..s.bands).any(|b| s.band[b].solo);
        let mut e = Self {
            sr,
            split_l: Splitter::default(),
            split_r: Splitter::default(),
            tuned_xover: s.xover_hz,
            coef: [ShelfCoef { gain_db: f64::NAN, ..Default::default() }; MAX_XOVERS],
            shelf_l: Default::default(),
            shelf_r: Default::default(),
            solver: GainSolver::new(),
            applied_shelf: [0.0; MAX_XOVERS],
            applied_top: 0.0,
            fade_shelf: [0.0; MAX_XOVERS],
            fade_top: 0.0,
            fade_left: 0,
            nb: s.bands,
            env: [0.0; MAX_BANDS],
            key_env: [0.0; MAX_BANDS],
            makeup: std::array::from_fn(|b| Glide { v: if s.band[b].bypass { 0.0 } else { s.band[b].makeup_db } }),
            full_w: Glide { v: if any_solo { 0.0 } else { 1.0 } },
            solo_w: std::array::from_fn(|b| Glide { v: if any_solo && s.band[b].solo { 1.0 } else { 0.0 } }),
            mix: Glide { v: s.mix },
            output: Glide { v: s.output_db },
        };
        e.split_l.tune(sr, &s.xover_hz);
        e.split_r.tune(sr, &s.xover_hz);
        e
    }

    /// Crossovers glide at block rate (¼ of the way per block) so a dragged
    /// handle sweeps instead of stepping.
    fn retune(&mut self, target: &[f32; MAX_XOVERS]) {
        let mut moved = false;
        for (t, &g) in self.tuned_xover.iter_mut().zip(target) {
            if (*t - g).abs() > g * 1e-4 {
                *t = if (*t - g).abs() < g * 0.002 { g } else { *t + (g - *t) * 0.25 };
                moved = true;
            }
        }
        if moved {
            let x = self.tuned_xover;
            self.split_l.tune(self.sr, &x);
            self.split_r.tune(self.sr, &x);
            for c in self.coef.iter_mut() { c.gain_db = f64::NAN; }
            self.solver.nb = 0;
        }
    }

    /// Band count changed while playing: every band whose range changed —
    /// the old top band, whose upper edge moves, and everything above it —
    /// starts from a silent detector (GR 0), and the audio gains glide from
    /// what was applied to the new layout instead of jumping (dropped shelves
    /// fade out). Resetting only the bands that came back left the old top
    /// band's wide-band envelope on a now-narrow band: 2 → 4 bands measured
    /// −15.7 dB on its first block against −10.5 dB at steady state.
    fn change_bands(&mut self, nb: usize) {
        for b in self.nb.min(nb).saturating_sub(1)..MAX_BANDS {
            self.env[b] = 0.0;
            self.key_env[b] = 0.0;
        }
        self.fade_shelf = self.applied_shelf;
        self.fade_top = self.applied_top;
        self.fade_left = (SWITCH_MS * 0.001 * self.sr) as u32;
        self.nb = nb;
    }

    /// Process one block. `key_db[b]` is the bus key's level over band `b`
    /// (−120 = no key / silent). All slices must be the same length.
    pub fn process(
        &mut self,
        s: &Settings,
        key_db: &[f32; MAX_BANDS],
        l_in: &[f32],
        r_in: &[f32],
        l_out: &mut [f32],
        r_out: &mut [f32],
    ) -> BlockReport {
        let s = s.sanitized(self.sr);
        self.retune(&s.xover_hz);
        let sr = self.sr;
        let nb = s.bands;
        if nb != self.nb { self.change_bands(nb); }
        if self.solver.nb != nb {
            let x = self.tuned_xover;
            self.solver.refresh(sr, &x, nb);
        }
        let k: [f64; MAX_XOVERS] = std::array::from_fn(|i| {
            1.0 / (core::f64::consts::PI * self.tuned_xover[i] as f64 / sr as f64).tan()
        });
        let atk: [f32; MAX_BANDS] = std::array::from_fn(|b| tc_coef(s.band[b].attack_ms, sr));
        let rel: [f32; MAX_BANDS] = std::array::from_fn(|b| tc_coef(s.band[b].release_ms, sr));
        // `key_db_at` interpolates bus bands and can hand back NaN or ±inf
        // when a neighbour is ±inf; through the one-pole that would latch
        // every keyed detector off until deactivate. Not-a-number and
        // anything at or under the silent line is silence; the top is capped.
        let key_lin: [f32; MAX_BANDS] = std::array::from_fn(|b| {
            if key_db[b] > KEY_SILENT_DB { 10f32.powf(key_db[b].min(KEY_MAX_DB) * 0.05) } else { 0.0 }
        });
        let any_solo = (0..nb).any(|b| s.band[b].solo);
        let solo_t: [f32; MAX_BANDS] = std::array::from_fn(|b| if any_solo && b < nb && s.band[b].solo { 1.0 } else { 0.0 });
        let glide = tc_coef(5.0, sr);
        let switch = tc_coef(SWITCH_MS, sr);
        let fade_len = ((SWITCH_MS * 0.001 * sr) as u32).max(1);

        let mut rep = BlockReport { gr_db: [0.0; MAX_BANDS], detector_db: [-120.0; MAX_BANDS] };
        let mut bl = [0.0f32; MAX_BANDS];
        let mut br = [0.0f32; MAX_BANDS];
        let n = l_in.len().min(r_in.len()).min(l_out.len()).min(r_out.len());
        for i in 0..n {
            let (xl, xr) = (l_in[i], r_in[i]);
            self.split_l.split(xl, nb, &mut bl);
            self.split_r.split(xr, nb, &mut br);

            let mut band_db = [0.0f32; MAX_BANDS];
            let mut gr_db = [0.0f32; MAX_BANDS];
            let mut mk_db = [0.0f32; MAX_BANDS];
            for b in 0..nb {
                let bs = &s.band[b];
                let peak = bl[b].abs().max(br[b].abs());
                let c = if peak > self.env[b] { atk[b] } else { rel[b] };
                self.env[b] = peak + (self.env[b] - peak) * c;
                let kc = if key_lin[b] > self.key_env[b] { atk[b] } else { rel[b] };
                self.key_env[b] = key_lin[b] + (self.key_env[b] - key_lin[b]) * kc;
                if !self.key_env[b].is_finite() { self.key_env[b] = 0.0; }
                let k = bs.key.clamp(0.0, 1.0);
                let det = self.env[b] * (1.0 - k) + self.key_env[b] * k;
                let det_db = 20.0 * det.max(1e-9).log10();
                gr_db[b] = if bs.bypass { 0.0 } else {
                    static_gain_db(det_db, bs.threshold_db, bs.ratio, bs.knee_db, bs.range_db)
                };
                mk_db[b] = self.makeup[b].step(if bs.bypass { 0.0 } else { bs.makeup_db }, glide);
                band_db[b] = gr_db[b] + mk_db[b];
                if i + 1 == n { rep.detector_db[b] = det_db; }
            }

            // The staircase runs even while soloed (output discarded), so its
            // shelves never resume from a frozen state.
            let (mut shelf, mut top) = self.solver.solve(&band_db);
            if self.fade_left > 0 {
                let a = 1.0 - self.fade_left as f64 / fade_len as f64;
                for c in 0..MAX_XOVERS { shelf[c] = self.fade_shelf[c] + (shelf[c] - self.fade_shelf[c]) * a; }
                top = self.fade_top + (top - self.fade_top) * a;
                self.fade_left -= 1;
            }
            self.applied_shelf = shelf;
            self.applied_top = top;
            let (stl, str_) = self.staircase(&shelf, top, &k, xl, xr);

            // Solo: crossfade between the full path and the soloed bands.
            let fw = self.full_w.step(if any_solo { 0.0 } else { 1.0 }, switch);
            let (mut wl, mut wr) = if fw == 1.0 { (stl, str_) } else { (stl * fw, str_ * fw) };
            for b in 0..MAX_BANDS {
                let w = self.solo_w[b].step(solo_t[b], switch);
                if w != 0.0 && b < nb {
                    let g = db_to_lin(band_db[b]) * w;
                    wl += bl[b] * g;
                    wr += br[b] * g;
                }
            }

            // Meter: the gain the shelves gave the band, not the request.
            for b in 0..nb {
                let err = if any_solo || self.fade_left > 0 { 0.0 } else { self.solver.applied[b] - band_db[b] as f64 };
                let g = if err.abs() < METER_ERR_DB { gr_db[b] } else { (self.solver.applied[b] - mk_db[b] as f64) as f32 };
                if g.abs() > rep.gr_db[b].abs() { rep.gr_db[b] = g; }
            }

            let mix = self.mix.step(s.mix.clamp(0.0, 1.0), glide);
            let og = db_to_lin(self.output.step(s.output_db, glide));
            l_out[i] = (xl + mix * (wl - xl)) * og;
            r_out[i] = (xr + mix * (wr - xr)) * og;
        }
        rep
    }

    /// Shelf gains at each crossover + the top band's gain as broadband.
    /// A 0 dB shelf is skipped (unity, state zeroed).
    #[inline]
    fn staircase(&mut self, shelf: &[f64; MAX_XOVERS], top: f64, k: &[f64; MAX_XOVERS], xl: f32, xr: f32) -> (f32, f32) {
        let (mut yl, mut yr) = (xl as f64, xr as f64);
        for c in 0..MAX_XOVERS {
            let d = shelf[c];
            let active = d != 0.0;
            // NaN (stale) fails the `<=` and forces a recompute.
            if active && !((self.coef[c].gain_db - d).abs() <= 1e-4) {
                self.coef[c].set(k[c], d);
            }
            yl = self.shelf_l[c].run(&self.coef[c], active, yl);
            yr = self.shelf_r[c].run(&self.coef[c], active, yr);
        }
        if top == 0.0 {
            (yl as f32, yr as f32)
        } else {
            let g = 10f64.powf(top / 20.0);
            ((yl * g) as f32, (yr * g) as f32)
        }
    }
}
