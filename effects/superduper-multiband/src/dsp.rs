//! The multiband engine, free of CLAP so tests drive it directly.
//!
//! Two signal paths, on purpose:
//!
//! ```text
//!   detector:  x ─► LR4 tree ─► band b ─► |·| max(L,R) ─► envelope ─┐
//!                                       key level (bus) ─► blend ───┴► curve ─► band dB
//!
//!   audio:     x ─► low shelf @c1 (dB0−dB1) ─► … ─► low shelf @c(n−1) ─► × 10^(dB(n−1)/20)
//! ```
//!
//! **Dynamic phase.** The audio never goes through the crossover. The band
//! gains are applied as a staircase of low shelves: the shelf at crossover
//! `c` carries the *difference* between the band below and the band above,
//! the top band's gain is a plain multiplier. Below `c1` the shelves multiply
//! out to band 0's gain, between `c1` and `c2` to band 1's, and so on. A
//! shelf whose difference is exactly 0 dB is skipped (output = input, state
//! zeroed — the exact state of a unity filter), so with no band compressing
//! the output is the input bit for bit. A band that compresses adds the phase
//! shift of one analog-style shelf pair, and only while it compresses — what
//! FabFilter calls Dynamic Phase. The shelves are 4th order, 24 dB/oct like
//! the detector's crossovers. A static LR4 sum would instead put a
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

impl Settings {
    /// Bands clamped to 2..=4, crossovers kept ascending with ≥ ⅓ octave
    /// between them and inside 20 Hz .. 0.45·sr.
    pub fn sanitized(mut self, sr: f32) -> Self {
        self.bands = self.bands.clamp(2, MAX_BANDS);
        let top = (sr * 0.45).min(20_000.0);
        let mut lo = 20.0f32;
        for c in self.xover_hz.iter_mut() {
            *c = c.clamp(lo, top);
            lo = (*c * 1.26).min(top);
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
    /// classic LR4 tree. The sum is flat in magnitude.
    #[inline]
    fn split(&mut self, x: f32, n: usize, out: &mut [f32; MAX_BANDS]) {
        let mut rest = x;
        for i in 0..n - 1 {
            out[i] = self.lp[i].run(rest);
            rest = self.hp[i].run(rest);
        }
        out[n - 1] = rest;
    }
}

// ---------------------------------------------------------------------------
// Audio path: 4th-order Butterworth low shelves in f64
// ---------------------------------------------------------------------------

/// Pole damping of the two sections of a 4th-order Butterworth:
/// sin((2m−1)·π/8), m = 1, 2.
const ZETA: [f64; 2] = [0.382_683_432_365_089_8, 0.923_879_532_511_286_7];

/// A 4th-order (24 dB/oct, matching the LR4 detector) low shelf: Butterworth
/// poles at radius G^(−1/8), zeros at G^(+1/8) around the crossover, so the
/// DC gain is G, the top is unity and the dB midpoint sits on the crossover.
/// A 2nd-order shelf leaked 0.3 dB two octaves out at 25 dB of GR.
#[derive(Default, Clone, Copy)]
struct ShelfCoef {
    /// Per section: b0, b1, b2, a1, a2 (normalised).
    sec: [[f64; 5]; 2],
    /// Gain these coefficients were computed for; NaN = stale.
    gain_db: f64,
}

impl ShelfCoef {
    /// `k` = 1 / tan(π·fc / sr), the bilinear transform prewarped at fc.
    fn set(&mut self, k: f64, gain_db: f64) {
        let rz = 10f64.powf(gain_db / 160.0); // G^(1/8)
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
    z: [[f64; 2]; 2],
}

impl ShelfState {
    /// Unity shelf: output = input, and the TDF-II state of a unity filter
    /// is zero — so engaging the shelf later starts from the right place.
    #[inline]
    fn run(&mut self, c: &ShelfCoef, active: bool, x: f64) -> f64 {
        if !active {
            self.z = [[0.0; 2]; 2];
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
    /// Most extreme gain change of the block (≤ 0 compressing, > 0 upward).
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
    env: [f32; MAX_BANDS],
    key_env: [f32; MAX_BANDS],
    makeup: [Glide; MAX_BANDS],
    mix: Glide,
    output: Glide,
}

impl Engine {
    pub fn new(sr: f32, s: &Settings) -> Self {
        let s = s.sanitized(sr);
        let mut e = Self {
            sr,
            split_l: Splitter::default(),
            split_r: Splitter::default(),
            tuned_xover: s.xover_hz,
            coef: [ShelfCoef { gain_db: f64::NAN, ..Default::default() }; MAX_XOVERS],
            shelf_l: Default::default(),
            shelf_r: Default::default(),
            env: [0.0; MAX_BANDS],
            key_env: [0.0; MAX_BANDS],
            makeup: std::array::from_fn(|b| Glide { v: if s.band[b].bypass { 0.0 } else { s.band[b].makeup_db } }),
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
        }
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
        let k: [f64; MAX_XOVERS] = std::array::from_fn(|i| {
            1.0 / (core::f64::consts::PI * self.tuned_xover[i] as f64 / sr as f64).tan()
        });
        let atk: [f32; MAX_BANDS] = std::array::from_fn(|b| tc_coef(s.band[b].attack_ms, sr));
        let rel: [f32; MAX_BANDS] = std::array::from_fn(|b| tc_coef(s.band[b].release_ms, sr));
        let key_lin: [f32; MAX_BANDS] = std::array::from_fn(|b| {
            if key_db[b] <= KEY_SILENT_DB { 0.0 } else { 10f32.powf(key_db[b] * 0.05) }
        });
        let any_solo = (0..nb).any(|b| s.band[b].solo);
        let glide = tc_coef(5.0, sr);

        let mut rep = BlockReport { gr_db: [0.0; MAX_BANDS], detector_db: [-120.0; MAX_BANDS] };
        let mut bl = [0.0f32; MAX_BANDS];
        let mut br = [0.0f32; MAX_BANDS];
        let n = l_in.len().min(r_in.len()).min(l_out.len()).min(r_out.len());
        for i in 0..n {
            let (xl, xr) = (l_in[i], r_in[i]);
            self.split_l.split(xl, nb, &mut bl);
            self.split_r.split(xr, nb, &mut br);

            let mut band_db = [0.0f32; MAX_BANDS];
            for b in 0..nb {
                let bs = &s.band[b];
                let peak = bl[b].abs().max(br[b].abs());
                let c = if peak > self.env[b] { atk[b] } else { rel[b] };
                self.env[b] = peak + (self.env[b] - peak) * c;
                let kc = if key_lin[b] > self.key_env[b] { atk[b] } else { rel[b] };
                self.key_env[b] = key_lin[b] + (self.key_env[b] - key_lin[b]) * kc;
                let k = bs.key.clamp(0.0, 1.0);
                let det = self.env[b] * (1.0 - k) + self.key_env[b] * k;
                let det_db = 20.0 * det.max(1e-9).log10();
                let gr = if bs.bypass { 0.0 } else {
                    static_gain_db(det_db, bs.threshold_db, bs.ratio, bs.knee_db, bs.range_db)
                };
                let mk = self.makeup[b].step(if bs.bypass { 0.0 } else { bs.makeup_db }, glide);
                band_db[b] = gr + mk;
                if gr.abs() > rep.gr_db[b].abs() { rep.gr_db[b] = gr; }
                if i + 1 == n { rep.detector_db[b] = det_db; }
            }

            let (wl, wr) = if any_solo {
                let (mut sl, mut sr_) = (0.0f32, 0.0f32);
                for b in (0..nb).filter(|&b| s.band[b].solo) {
                    let g = db_to_lin(band_db[b]);
                    sl += bl[b] * g;
                    sr_ += br[b] * g;
                }
                (sl, sr_)
            } else {
                self.staircase(&band_db, nb, &k, xl, xr)
            };

            let mix = self.mix.step(s.mix.clamp(0.0, 1.0), glide);
            let og = db_to_lin(self.output.step(s.output_db, glide));
            l_out[i] = (xl + mix * (wl - xl)) * og;
            r_out[i] = (xr + mix * (wr - xr)) * og;
        }
        rep
    }

    /// Band gains → shelves at each crossover + the top band as broadband.
    #[inline]
    fn staircase(&mut self, band_db: &[f32; MAX_BANDS], nb: usize, k: &[f64; MAX_XOVERS], xl: f32, xr: f32) -> (f32, f32) {
        let (mut yl, mut yr) = (xl as f64, xr as f64);
        for c in 0..nb - 1 {
            let d = (band_db[c] - band_db[c + 1]) as f64;
            let active = d != 0.0;
            // NaN (stale) fails the `<=` and forces a recompute.
            if active && !((self.coef[c].gain_db - d).abs() <= 1e-4) {
                self.coef[c].set(k[c], d);
            }
            yl = self.shelf_l[c].run(&self.coef[c], active, yl);
            yr = self.shelf_r[c].run(&self.coef[c], active, yr);
        }
        // Unused shelves (fewer bands) idle at the unity state.
        for c in nb.saturating_sub(1)..MAX_XOVERS {
            self.shelf_l[c] = ShelfState::default();
            self.shelf_r[c] = ShelfState::default();
        }
        let top = band_db[nb - 1];
        if top == 0.0 {
            (yl as f32, yr as f32)
        } else {
            let g = 10f64.powf(top as f64 / 20.0);
            ((yl * g) as f32, (yr * g) as f32)
        }
    }
}
