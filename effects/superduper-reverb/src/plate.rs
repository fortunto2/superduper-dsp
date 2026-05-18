//! Dattorro plate reverb (1997 "Effect Design Part 1: Reverberator and Other
//! Filters", figure 8 — "plate reverberator").
//!
//! Topology:
//!
//! ```text
//!   input → bandwidth-LPF → input diffuser (4 allpasses in series, gains 0.75/0.625)
//!         → splits into two tanks (figure-of-eight crossfeed):
//!
//!     L-tank: mod-allpass(672 ±mod, 0.7) → delay(4453) → damping LPF →
//!             allpass(1800, 0.5) → delay(3720) → feedback to R-tank
//!     R-tank: mod-allpass(908 ±mod, 0.7) → delay(4217) → damping LPF →
//!             allpass(2656, 0.5) → delay(3163) → feedback to L-tank
//!
//!   Output L = sum of 7 taps from both tanks at hand-tuned offsets.
//!   Output R = mirror.
//! ```
//!
//! Why this beats four-comb-Schroeder by a long way:
//!   - the modulated allpass in each tank breaks resonant peaks → smoother tail
//!   - figure-of-eight crossfeed = real stereo width (one ear can hear what
//!     the other tank is doing, glued together by the decay loop)
//!   - 7-tap output picks samples at non-harmonic offsets → no comb-tooth ring
//!   - damping LPF lives IN the feedback loop, so HF actually rolls off in
//!     the tail (not just on the output)
//!
//! All sample-position constants are quoted at Dattorro's 29.761 kHz reference
//! rate (Lexicon Tape) and scaled at runtime by `sr / 29761` × `size`.

// Buffer sizes — sized for SR up to 96 kHz with Size up to 1.5×, plus headroom
// for modulation. 16384 is comfortably bigger than 4453 × (96000/29761) × 1.5
// ≈ 10780.
const TANK_BUF: usize = 16384;
const AP_BUF: usize = 4096;

// Dattorro's published lengths at 29.761 kHz.
const DIFF_LENS: [f32; 4] = [142.0, 107.0, 379.0, 277.0];
const DIFF_GAINS: [f32; 4] = [0.75, 0.75, 0.625, 0.625];

const TANK_MOD_LEN_L: f32 = 672.0;
const TANK_MOD_LEN_R: f32 = 908.0;
const TANK_MOD_GAIN: f32 = 0.7;

const TANK_DELAY1_L: f32 = 4453.0;
const TANK_DELAY1_R: f32 = 4217.0;

const TANK_AP2_L: f32 = 1800.0;
const TANK_AP2_R: f32 = 2656.0;
const TANK_AP2_GAIN: f32 = 0.5;

const TANK_DELAY2_L: f32 = 3720.0;
const TANK_DELAY2_R: f32 = 3163.0;

const REF_SR: f32 = 29761.0;

// Mod excursion at reference rate (samples). Scaled by SR + Modulation knob.
const MOD_EXCURSION: f32 = 8.0;

// ---------------------------------------------------------------------------
// Building blocks
// ---------------------------------------------------------------------------

/// 3rd-order Lagrange interpolation read from a ring buffer. Used by every
/// in-tank delay tap so a continuous SIZE sweep produces no clicks AND
/// preserves HF in the reverb tail. Linear interp is enough to silence the
/// click but adds a per-tap low-pass that compounds badly over a long
/// figure-of-eight decay — Lagrange-3 is "maximally flat at DC", costing
/// 3 extra multiplies per read for a ~50× tighter HF response than linear.
/// Reference: J.O. Smith, "Physical Audio Signal Processing",
/// "Lagrange Interpolation".
///
/// Requires `1.0 <= d <= cap - 3` so the four taps stay inside the buffer.
#[inline]
fn lagrange3_read(buf: &[f32], write_idx: usize, cap: usize, d: f32) -> f32 {
    let d = d.max(1.0).min((cap - 3) as f32);
    let d_int = d as usize;
    let frac = d - d_int as f32;
    let base = (write_idx + cap - d_int - 1) % cap;
    let y_m1 = buf[base];
    let y_0 = buf[(base + 1) % cap];
    let y_1 = buf[(base + 2) % cap];
    let y_2 = buf[(base + 3) % cap];
    let c0 = -frac * (frac - 1.0) * (frac - 2.0) / 6.0;
    let c1 = (frac + 1.0) * (frac - 1.0) * (frac - 2.0) / 2.0;
    let c2 = -(frac + 1.0) * frac * (frac - 2.0) / 2.0;
    let c3 = (frac + 1.0) * frac * (frac - 1.0) / 6.0;
    c0 * y_m1 + c1 * y_0 + c2 * y_1 + c3 * y_2
}

struct Allpass {
    buf: [f32; AP_BUF],
    idx: usize,
}

impl Default for Allpass {
    fn default() -> Self {
        Self { buf: [0.0; AP_BUF], idx: 0 }
    }
}

impl Allpass {
    /// Schroeder allpass with Lagrange-3 interpolated tap so the delay
    /// length can be swept continuously (SIZE knob ramps `len_frac`
    /// between integer values; without interpolation the read pointer
    /// jumps whole samples and the feedback loop emits an audible click).
    ///   v[n] = x[n] + g * v[n-D]   (state, written into buf)
    ///   y[n] = -g * v[n] + v[n-D]  (output — has DC gain 1.0)
    fn process(&mut self, x: f32, len_frac: f32, g: f32) -> f32 {
        let delayed = lagrange3_read(&self.buf, self.idx, AP_BUF, len_frac);
        let v = x + g * delayed;
        let y = -g * v + delayed;
        self.buf[self.idx] = v;
        self.idx = (self.idx + 1) % AP_BUF;
        y
    }
    /// Read from inside the delay line at an arbitrary fractional offset.
    /// Used by the 7-tap output — the SIZE knob scales every offset, so
    /// any non-interpolated tap clicks when its position crosses an
    /// integer.
    fn tap_buf(&self, offset: f32) -> f32 {
        lagrange3_read(&self.buf, self.idx, AP_BUF, offset)
    }
}

struct ModAllpass {
    buf: [f32; AP_BUF],
    idx: usize,
}

impl Default for ModAllpass {
    fn default() -> Self {
        Self { buf: [0.0; AP_BUF], idx: 0 }
    }
}

impl ModAllpass {
    /// Allpass with Lagrange-3 interpolated tap (so we can sweep delay
    /// length smoothly via the LFO). Same Schroeder structure as
    /// `Allpass::process`. Upgraded from linear interp when the same fix
    /// landed on the fixed allpasses — keeping all allpasses on the same
    /// interpolation kernel preserves the tank's frequency response.
    fn process(&mut self, x: f32, len_frac: f32, g: f32) -> f32 {
        let delayed = lagrange3_read(&self.buf, self.idx, AP_BUF, len_frac);
        let v = x + g * delayed;
        let y = -g * v + delayed;
        self.buf[self.idx] = v;
        self.idx = (self.idx + 1) % AP_BUF;
        y
    }
}

struct Delay {
    buf: Box<[f32; TANK_BUF]>,
    idx: usize,
}

impl Default for Delay {
    fn default() -> Self {
        Self { buf: Box::new([0.0; TANK_BUF]), idx: 0 }
    }
}

impl Delay {
    fn write(&mut self, x: f32) {
        self.buf[self.idx] = x;
    }
    fn advance(&mut self) {
        self.idx = (self.idx + 1) % TANK_BUF;
    }
    /// Fractional tap with Lagrange-3 interpolation. Required for click-
    /// free SIZE sweeps — `size` scales every tap offset, and integer-
    /// only reads jump whole samples when the offset crosses an integer.
    /// Lagrange-3 (vs linear) preserves the reverb's HF content over the
    /// long feedback loop — see `lagrange3_read` for the rationale.
    fn tap(&self, offset: f32) -> f32 {
        lagrange3_read(self.buf.as_slice(), self.idx, TANK_BUF, offset)
    }
}

// ---------------------------------------------------------------------------
// PlateState — full stereo tank
// ---------------------------------------------------------------------------

pub struct PlateState {
    // DC blocker at the input — without this, DC drift accumulates inside
    // the figure-of-eight feedback loop and eventually drowns the audible
    // tail in a quiet rumble. Cheap one-pole HPF.
    dc: superduper_synth_core::dsp_blocks::DcBlocker,

    // Input bandwidth one-pole (rolls top end before the tank).
    bandwidth_lp: f32,

    // 4-stage input diffuser.
    diff: [Allpass; 4],

    // Two tanks.
    tank_l_mod_ap: ModAllpass,
    tank_l_delay1: Delay,
    tank_l_damp_lp: f32,
    tank_l_ap2: Allpass,
    tank_l_delay2: Delay,

    tank_r_mod_ap: ModAllpass,
    tank_r_delay1: Delay,
    tank_r_damp_lp: f32,
    tank_r_ap2: Allpass,
    tank_r_delay2: Delay,

    // Crossfeed state — last sample written into each tank delay2 (used as
    // input to the OTHER tank's mod_ap on the next iteration).
    cross_l_to_r: f32,
    cross_r_to_l: f32,

    // LFOs (one per tank, slightly detuned).
    lfo_l_phase: f32,
    lfo_r_phase: f32,

    // Pre-delay (stereo).
    predelay_l: Box<[f32; PREDELAY_MAX]>,
    predelay_r: Box<[f32; PREDELAY_MAX]>,
    predelay_idx: usize,
}

pub const PREDELAY_MAX: usize = 96_000; // 1 s @ 96k, 2 s @ 48k

// Re-export Ducker from the shared library so existing call sites
// (`use superduper_reverb::plate::Ducker`) keep working transparently.
pub use superduper_synth_core::dsp_blocks::Ducker;

impl Default for PlateState {
    fn default() -> Self {
        Self {
            dc: Default::default(),
            bandwidth_lp: 0.0,
            diff: Default::default(),
            tank_l_mod_ap: Default::default(),
            tank_l_delay1: Default::default(),
            tank_l_damp_lp: 0.0,
            tank_l_ap2: Default::default(),
            tank_l_delay2: Default::default(),
            tank_r_mod_ap: Default::default(),
            tank_r_delay1: Default::default(),
            tank_r_damp_lp: 0.0,
            tank_r_ap2: Default::default(),
            tank_r_delay2: Default::default(),
            cross_l_to_r: 0.0,
            cross_r_to_l: 0.0,
            lfo_l_phase: 0.0,
            lfo_r_phase: 0.0,
            predelay_l: Box::new([0.0; PREDELAY_MAX]),
            predelay_r: Box::new([0.0; PREDELAY_MAX]),
            predelay_idx: 0,
        }
    }
}

/// Tuning passed to `process` each block (recomputed cheaply from CLAP params).
#[derive(Copy, Clone)]
pub struct PlateParams {
    pub sr: f32,
    pub size: f32,        // 0.1..1.5 → scales every tank delay
    pub decay: f32,       // 0..0.95 → feedback gain after each tank
    pub damp: f32,        // 0..1 → in-loop LPF coefficient (HF kill)
    pub bandwidth: f32,   // 0..1 → input LPF (1 = full bandwidth, 0 = dark)
    pub predelay_ms: f32, // 0..200
    pub modulation: f32,  // 0..1 → LFO excursion depth
}

impl PlateState {
    /// Process one stereo sample. Returns (wet_l, wet_r). Caller mixes with dry.
    #[inline]
    pub fn process_sample(&mut self, in_l: f32, in_r: f32, p: PlateParams) -> (f32, f32) {
        let scale = (p.sr / REF_SR) * p.size;

        // ---- Pre-delay ----
        let pd_samples = ((p.predelay_ms * 0.001 * p.sr) as usize).clamp(0, PREDELAY_MAX - 1);
        self.predelay_l[self.predelay_idx] = in_l;
        self.predelay_r[self.predelay_idx] = in_r;
        let pd_read = (self.predelay_idx + PREDELAY_MAX - pd_samples) % PREDELAY_MAX;
        let pre_l = self.predelay_l[pd_read];
        let pre_r = self.predelay_r[pd_read];
        self.predelay_idx = (self.predelay_idx + 1) % PREDELAY_MAX;

        // Sum to mono going into the diffuser (Dattorro's plate is mono-summed
        // at the input — stereo is created by the figure-of-eight, not by
        // independent channels).
        let mut x = (pre_l + pre_r) * 0.5;

        // ---- DC removal ----
        // Critical for ambient reverbs: DC bleeds through allpasses unchanged
        // and accumulates in the feedback loop. Block it before the tank.
        x = self.dc.process(x);

        // ---- Input bandwidth LPF (one-pole) ----
        let bw_coef = 1.0 - p.bandwidth.clamp(0.0, 1.0);
        self.bandwidth_lp = x * (1.0 - bw_coef) + self.bandwidth_lp * bw_coef;
        x = self.bandwidth_lp;

        // ---- Input diffuser: 4 cascaded allpasses ----
        for i in 0..4 {
            let len = DIFF_LENS[i] * scale;
            x = self.diff[i].process(x, len, DIFF_GAINS[i]);
        }

        // ---- LFOs (one per tank, slightly detuned) ----
        let lfo_inc_l = core::f32::consts::TAU * 0.81 / p.sr;
        let lfo_inc_r = core::f32::consts::TAU * 0.97 / p.sr;
        self.lfo_l_phase += lfo_inc_l;
        if self.lfo_l_phase >= core::f32::consts::TAU {
            self.lfo_l_phase -= core::f32::consts::TAU;
        }
        self.lfo_r_phase += lfo_inc_r;
        if self.lfo_r_phase >= core::f32::consts::TAU {
            self.lfo_r_phase -= core::f32::consts::TAU;
        }
        let excursion = MOD_EXCURSION * scale * p.modulation;
        let mod_l_len = TANK_MOD_LEN_L * scale + excursion * self.lfo_l_phase.sin();
        let mod_r_len = TANK_MOD_LEN_R * scale + excursion * self.lfo_r_phase.sin();

        // Loop gain = decay, applied ONCE per full L→R→L round trip.
        // Following Dattorro figure 8: decay is the single multiplier between
        // tank_delay2 output and the OTHER tank's input. Inside the tank,
        // the damping LPF passes DC at unity — so total loop gain at DC ≈ decay.
        // decay ∈ [0, 0.95] keeps it stable.

        // ---- L-tank ----
        let l_in = x + self.cross_r_to_l;
        let l_after_mod = self.tank_l_mod_ap.process(l_in, mod_l_len, TANK_MOD_GAIN);
        self.tank_l_delay1.write(l_after_mod);
        let d1_len_l = TANK_DELAY1_L * scale;
        let after_d1_l = self.tank_l_delay1.tap(d1_len_l);
        self.tank_l_damp_lp = after_d1_l * (1.0 - p.damp) + self.tank_l_damp_lp * p.damp;
        let ap2_len_l = TANK_AP2_L * scale;
        let after_ap2_l = self.tank_l_ap2.process(self.tank_l_damp_lp, ap2_len_l, TANK_AP2_GAIN);
        self.tank_l_delay2.write(after_ap2_l);
        let d2_len_l = TANK_DELAY2_L * scale;
        let after_d2_l = self.tank_l_delay2.tap(d2_len_l);
        // Crossfeed gain = decay (the single loop attenuation).
        self.cross_l_to_r = after_d2_l * p.decay;

        // ---- R-tank ----
        let r_in = x + self.cross_l_to_r;
        let r_after_mod = self.tank_r_mod_ap.process(r_in, mod_r_len, TANK_MOD_GAIN);
        self.tank_r_delay1.write(r_after_mod);
        let d1_len_r = TANK_DELAY1_R * scale;
        let after_d1_r = self.tank_r_delay1.tap(d1_len_r);
        self.tank_r_damp_lp = after_d1_r * (1.0 - p.damp) + self.tank_r_damp_lp * p.damp;
        let ap2_len_r = TANK_AP2_R * scale;
        let after_ap2_r = self.tank_r_ap2.process(self.tank_r_damp_lp, ap2_len_r, TANK_AP2_GAIN);
        self.tank_r_delay2.write(after_ap2_r);
        let d2_len_r = TANK_DELAY2_R * scale;
        let after_d2_r = self.tank_r_delay2.tap(d2_len_r);
        self.cross_r_to_l = after_d2_r * p.decay;

        // Advance the write index of every delay line we wrote to.
        self.tank_l_delay1.advance();
        self.tank_l_delay2.advance();
        self.tank_r_delay1.advance();
        self.tank_r_delay2.advance();

        // ---- Multi-tap output (Dattorro's published offsets, scaled) ----
        // Left output reads mostly from R-tank (and vice versa) — that's how
        // we get the broad stereo image from a figure-of-eight reverb. All
        // taps are fractional → SIZE can sweep continuously without clicks.
        let out_l = self.tank_r_delay1.tap(266.0 * scale)
            + self.tank_r_delay1.tap(2974.0 * scale)
            - self.tank_r_ap2.tap_buf(1913.0 * scale)
            + self.tank_r_delay2.tap(1996.0 * scale)
            - self.tank_l_delay1.tap(1990.0 * scale)
            - self.tank_l_ap2.tap_buf(187.0 * scale)
            - self.tank_l_delay2.tap(1066.0 * scale);

        let out_r = self.tank_l_delay1.tap(353.0 * scale)
            + self.tank_l_delay1.tap(3627.0 * scale)
            - self.tank_l_ap2.tap_buf(1228.0 * scale)
            + self.tank_l_delay2.tap(2673.0 * scale)
            - self.tank_r_delay1.tap(2111.0 * scale)
            - self.tank_r_ap2.tap_buf(335.0 * scale)
            - self.tank_r_delay2.tap(121.0 * scale);

        // Dattorro normalises by ~0.6, then we leave headroom for downstream
        // mixing. Empirically this lands around -3 dBFS at unity decay so the
        // wet signal isn't disproportionately louder than dry.
        (out_l * 0.6, out_r * 0.6)
    }
}

