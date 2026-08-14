//! Procedural HOWLING-WIND engine — Andy Farnell's "Designing Sound" wind
//! model: broadband noise through 2-3 high-Q resonant bandpass filters
//! whose centre frequencies slowly sweep (independent LFO + random walk),
//! spanning roughly 200 Hz-2 kHz. This is the pitched "whoooo" hiss that
//! makes wind sound like it's actually howling («завывание»), as opposed
//! to the gentle formant-bandpassed breath noise in `voice.rs`'s original
//! airy layer (which this engine is blended against via the `Howl` param).
//!
//! GUST surges (the slow amplitude swell that makes the howl "surge") are
//! deliberately NOT owned by this engine — a real gust doesn't hit each
//! polyphonic voice independently, so `lib.rs` computes ONE shared gust
//! envelope per block and applies it uniformly (both to every voice's howl
//! layer and to Overlay's wind-bed + input-ducking filter).
//!
//! On top of the broadband howl, `AeolianTone` adds a physically-derived
//! **vortex-shedding whistle** — the tonal edge you hear when wind passes a
//! wire, branch, or gap (an Aeolian tone). Frequency comes from the
//! **Strouhal relation** `f = St·U/d` (St ≈ 0.2 for a cylinder in the
//! relevant Reynolds range): as the virtual wind speed `U` rises with the
//! gust, `f` glides upward — the characteristic "whistling up" of a gust
//! hitting a wire. Blended in via the `Whistle` param.

use crate::noise::{ColorNoise, WobbleGen};
use superduper_synth_core::dsp_blocks::Biquad;

const N_BANDS: usize = 3;

/// Base centre frequencies (Hz) at `pitch_mult` = 1 — log-spaced across the
/// ~200 Hz-2 kHz range Farnell's model targets.
const BASE_HZ: [f32; N_BANDS] = [280.0, 650.0, 1400.0];
/// Distinct sweep-LFO rates per band (Hz), within the requested 0.1-2 Hz
/// range and deliberately non-multiples of each other so the three bands
/// don't lock into a repeating combined pattern.
const LFO_HZ: [f32; N_BANDS] = [0.17, 0.41, 0.29];
/// Random-walk wander rate per band — slower than its LFO, layered on top
/// for the "not quite periodic" organic howl character real wind has.
const WALK_HZ: [f32; N_BANDS] = [0.09, 0.21, 0.15];

/// How many samples a filter's (trig-heavy) coefficients stay cached
/// before being recomputed. The sweep moves at <2 Hz, so 8 samples
/// (>2.7 kHz effective update rate at 48 kHz) is enormously oversampled —
/// this just keeps the per-sample cost sane at high voice counts.
const COEF_HOLD: u8 = 8;

struct HowlFilter {
    biquad: Biquad,
    lfo_phase: f32,
    walk: WobbleGen,
    base_hz: f32,
    lfo_hz: f32,
    walk_hz: f32,
    hold: u8,
}

impl HowlFilter {
    fn new(seed: u32, base_hz: f32, lfo_hz: f32, walk_hz: f32) -> Self {
        Self {
            biquad: Biquad::default(),
            // Integer hash → [0,1). `(seed as f32 * 0.618).fract()` looked like
            // a golden-ratio scatter but returned exactly 0.0 for every seed the
            // caller actually uses: seeds come from a `wrapping_mul` hash, so
            // they land above 2^24 where f32 has no fractional bits left, and
            // `.fract()` of a huge integer-valued float is 0. All three bands
            // therefore started phase-locked and swept together — the opposite of
            // the decorrelation this field exists for.
            lfo_phase: (seed >> 8) as f32 / 16_777_216.0,
            walk: WobbleGen::new(seed),
            base_hz,
            lfo_hz,
            walk_hz,
            hold: 0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32, sr: f32, q: f32, sweep_depth: f32, pitch_mult: f32) -> f32 {
        self.lfo_phase += self.lfo_hz / sr;
        if self.lfo_phase >= 1.0 {
            self.lfo_phase -= 1.0;
        }
        if self.hold == 0 {
            self.hold = COEF_HOLD;
            let lfo = (self.lfo_phase * core::f32::consts::TAU).sin();
            let walk = self.walk.next(sr, self.walk_hz);
            let excursion = 0.6 * lfo + 0.4 * walk;
            let center = (self.base_hz * pitch_mult * (1.0 + excursion * sweep_depth))
                .clamp(90.0, (sr * 0.45 * 0.9).max(200.0));
            self.biquad.set_bandpass(sr, center, q.max(0.5));
        } else {
            self.hold -= 1;
        }
        self.biquad.process(x)
    }
}

// ---------------------------------------------------------------------------
// Aeolian tone — vortex-shedding whistle via the Strouhal relation.
// ---------------------------------------------------------------------------

/// Strouhal number for a cylinder/wire in the relevant Reynolds range —
/// the standard textbook constant (~0.2) for vortex-shedding / Aeolian
/// tones (telegraph-wire whistling, flagpole howl, etc).
const STROUHAL: f32 = 0.2;
/// Virtual wind-speed range (arbitrary units) the gust envelope maps onto —
/// tuned together with `D_BASE` so `f = St·U/d` lands in an audible
/// whistle range (see `whistle_freq_hz` below).
const WIND_SPEED_MIN: f32 = 4.0;
const WIND_SPEED_MAX: f32 = 16.0;
/// "Obstacle size" reference (arbitrary units) at `pitch_mult` = 1.
/// Divided by `pitch_mult` so a played note transposes the whistle too —
/// same convention `HowlFilter` uses for the broadband sweep range.
const OBSTACLE_D_BASE: f32 = 0.0016;
/// Narrow, tonal Q for the whistle — much tighter than the broadband howl
/// bands (Q 2-10) since a vortex-shedding tone is close to a pure whistle.
const AEOLIAN_Q: f32 = 14.0;

/// `f = St·U/d` — U driven by the current wind intensity (0..1, typically
/// the shared gust multiplier), d by the played note via `pitch_mult`.
#[inline]
fn aeolian_freq_hz(sr: f32, intensity01: f32, pitch_mult: f32) -> f32 {
    let u = WIND_SPEED_MIN + (WIND_SPEED_MAX - WIND_SPEED_MIN) * intensity01.clamp(0.0, 1.0);
    let d = (OBSTACLE_D_BASE / pitch_mult.max(0.05)).max(1e-6);
    (STROUHAL * u / d).clamp(150.0, (sr * 0.45).max(200.0))
}

struct AeolianTone {
    bp_l: Biquad,
    bp_r: Biquad,
    hold: u8,
}

impl AeolianTone {
    fn new() -> Self {
        Self {
            bp_l: Biquad::default(),
            bp_r: Biquad::default(),
            hold: 0,
        }
    }

    #[inline]
    fn process(&mut self, noise_l: f32, noise_r: f32, sr: f32, freq_hz: f32) -> (f32, f32) {
        if self.hold == 0 {
            self.hold = COEF_HOLD;
            self.bp_l.set_bandpass(sr, freq_hz, AEOLIAN_Q);
            self.bp_r.set_bandpass(sr, freq_hz, AEOLIAN_Q);
        } else {
            self.hold -= 1;
        }
        (self.bp_l.process(noise_l), self.bp_r.process(noise_r))
    }
}

/// One stereo howling-wind voice: one noise source per channel feeding
/// three swept resonant bandpasses (Farnell's topology) plus an optional
/// Aeolian-tone whistle, summed.
pub struct HowlEngine {
    filters_l: [HowlFilter; N_BANDS],
    filters_r: [HowlFilter; N_BANDS],
    noise_l: ColorNoise,
    noise_r: ColorNoise,
    aeolian: AeolianTone,
    /// Last computed whistle frequency — exposed via `last_whistle_hz` for
    /// tests/telemetry so the "it glides with the gust" claim is checkable
    /// without re-deriving the Strouhal formula from the outside.
    last_whistle_hz: f32,
}

impl HowlEngine {
    pub fn new(seed: u32) -> Self {
        let mk = |ch_seed: u32| -> [HowlFilter; N_BANDS] {
            std::array::from_fn(|i| {
                HowlFilter::new(
                    ch_seed
                        .wrapping_add(i as u32 * 7919)
                        .wrapping_mul(2_654_435_761)
                        .max(1),
                    BASE_HZ[i],
                    LFO_HZ[i],
                    WALK_HZ[i],
                )
            })
        };
        Self {
            filters_l: mk(seed ^ 0x51ED_270B),
            filters_r: mk(seed ^ 0xA13F_C965),
            noise_l: ColorNoise::new(seed ^ 0x2545_F491),
            noise_r: ColorNoise::new(seed ^ 0x9E37_79B9),
            aeolian: AeolianTone::new(),
            last_whistle_hz: 0.0,
        }
    }

    /// Render one stereo sample. `howl` (0..1) sets BOTH the resonance (Q)
    /// and the sweep excursion of the broadband bands — 0 = broad/gentle/
    /// barely swept, 1 = tight resonant bands sweeping widely (full howl).
    /// `pitch_mult` transposes the whole band set AND the Aeolian whistle
    /// (1.0 = base range; this is how a played MIDI note "tunes" the howl,
    /// or how Overlay loosely follows input pitch). `whistle` (0..1) blends
    /// in the vortex-shedding tone; `gust_mult` (0..1, the shared gust
    /// envelope) drives its Strouhal wind speed `U` — the tone glides up in
    /// pitch AND gets louder as the gust surges, gated by `howl` (no
    /// whistle in the gentle-breath end of the Howl range).
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub fn process(
        &mut self,
        sr: f32,
        howl: f32,
        color: f32,
        pitch_mult: f32,
        whistle: f32,
        gust_mult: f32,
    ) -> (f32, f32) {
        let howl = howl.clamp(0.0, 1.0);
        let q = 2.0 + howl * 8.0;
        let sweep_depth = 0.15 + howl * 0.75;
        let raw_l = self.noise_l.next(color);
        let raw_r = self.noise_r.next(color);
        let mut l = 0.0_f32;
        let mut r = 0.0_f32;
        for f in self.filters_l.iter_mut() {
            l += f.process(raw_l, sr, q, sweep_depth, pitch_mult);
        }
        for f in self.filters_r.iter_mut() {
            r += f.process(raw_r, sr, q, sweep_depth, pitch_mult);
        }
        // Makeup gain — summing 3 moderate-Q resonant bandpasses leaves
        // plenty of headroom; scale so Howl=1 lands near the same
        // perceived loudness as the old breath layer did at Breath=1.
        l *= 0.9;
        r *= 0.9;

        let whistle = whistle.clamp(0.0, 1.0);
        if whistle > 1e-4 && howl > 1e-4 {
            let intensity = gust_mult.clamp(0.0, 1.0);
            let freq_hz = aeolian_freq_hz(sr, intensity, pitch_mult);
            self.last_whistle_hz = freq_hz;
            let (al, ar) = self.aeolian.process(raw_l, raw_r, sr, freq_hz);
            // Amplitude rises with wind speed too — a gust doesn't just
            // raise the whistle's pitch, it makes it louder (0.35 floor so
            // it doesn't vanish between gusts, full swing to 1.0 at peak).
            let amt = whistle * howl * (0.35 + 0.65 * intensity) * 2.4;
            l += al * amt;
            r += ar * amt;
        }
        (l, r)
    }

    /// Last Strouhal-derived whistle frequency (Hz) — 0.0 if the whistle
    /// hasn't fired yet (Whistle/Howl both need to be > 0). Read-only
    /// telemetry for tests/GUI, not used by the audio path itself.
    pub fn last_whistle_hz(&self) -> f32 {
        self.last_whistle_hz
    }
}
