//! One double: a TD-PSOLA synthesis head reading the shared input ring at its
//! own drifting delay and its own drifting pitch ratio.
//!
//! Why PSOLA and not a modulated delay line: a delay that moves 30 ms over a
//! second is a 3 % speed change, i.e. a Doppler shift of about ±50 cents —
//! a chorus, not a double. Here the delay only chooses WHICH analysis grain
//! (pitch mark) a synthesis grain copies, and the pitch comes from the spacing
//! of the synthesis marks alone. Time and pitch are separate axes, the way two
//! takes of one singer differ, and the grain content is copied unchanged so the
//! formants ride along (or move by `beta` when asked).

use super::noise::{mix_seed, SmoothNoise};
use super::Source;
use crate::scale;

/// Pitch below which a tracked estimate is not trusted for snapping.
const VOICED_HZ: f32 = 55.0;
/// Largest snap a Tuned double will make (an octave error must not throw it).
const MAX_CORR_ST: f32 = 12.0;
/// Fraction of the Timing range that is the double's minimum lag. A double is
/// never exactly on the lead (that is a comb filter), and never a fixed offset.
pub const DELAY_FLOOR: f32 = 0.3;

pub struct DoubleVoice {
    out: Box<[f32]>,
    win: Box<[f32]>,
    omask: usize,
    next_synth: f64,
    next_analysis: f64,
    time_noise: SmoothNoise,
    pitch_noise: SmoothNoise,
    /// Smoothed delay behind the lead, samples.
    delay: f32,
    /// Smoothed natural pitch offset, cents.
    cents: f32,
    /// Tuned-mode correction, semitones (retune-smoothed at grain rate).
    corr_st: f32,
    /// What the last grain was actually shifted by, semitones — the readout.
    applied_st: f32,
    /// Fixed per-seed level offset, linear (≤ 1: doubles sit slightly lower).
    pub level: f32,
    /// Fade gain 0..1, linear ramp so it reaches exactly 0.
    pub gain: f32,
    pub active: bool,
}

/// What a voice needs from the parameters, per sample.
#[derive(Clone, Copy)]
pub struct VoiceCtl {
    /// Max delay, samples (the Timing param).
    pub timing: f32,
    /// Natural pitch wander depth, cents.
    pub var_cents: f32,
    /// Formant read ratio.
    pub beta: f32,
    /// `Some((key, mask, retune_samples))` snaps the double to the scale.
    pub tuned: Option<(u8, u16, f32)>,
    /// One-pole coefficient for delay / cents smoothing.
    pub smooth: f32,
}

impl DoubleVoice {
    pub fn new(out_len: usize, sr: f32, seed: u32, index: u32) -> Self {
        let len = out_len.next_power_of_two();
        let mut v = Self {
            out: vec![0.0; len].into_boxed_slice(),
            win: vec![0.0; len].into_boxed_slice(),
            omask: len - 1,
            next_synth: 0.0,
            next_analysis: 0.0,
            time_noise: SmoothNoise::new(1, 0.5, sr),
            pitch_noise: SmoothNoise::new(2, 0.5, sr),
            delay: 0.0,
            cents: 0.0,
            corr_st: 0.0,
            applied_st: 0.0,
            level: 1.0,
            gain: 0.0,
            active: false,
        };
        v.reseed(seed, index, sr);
        v
    }

    /// Re-derive the voice's character from `seed`: its two wander rates
    /// (0.3–1 Hz, independent for time and pitch) and its level offset.
    pub fn reseed(&mut self, seed: u32, index: u32, sr: f32) {
        let mut r = crate::dsp_blocks::Xorshift::new(mix_seed(seed, index, 0));
        let t_rate = 0.3 + 0.7 * r.next_f32();
        let p_rate = 0.3 + 0.7 * r.next_f32();
        self.level = 10f32.powf(-2.5 * r.next_f32() / 20.0);
        self.time_noise = SmoothNoise::new(mix_seed(seed, index, 1), t_rate, sr);
        self.pitch_noise = SmoothNoise::new(mix_seed(seed, index, 2), p_rate, sr);
    }

    /// Silent and not wanted: costs nothing until woken.
    #[inline]
    pub fn dormant(&self) -> bool {
        !self.active && self.gain <= 0.0
    }

    /// Start from cold at output position `r`: empty rings, the first grain one
    /// max-period ahead so it is placed before it is read.
    pub fn wake(&mut self, r: i64, t0_max: f32, ctl: &VoiceCtl) {
        self.out.fill(0.0);
        self.win.fill(0.0);
        self.next_synth = (r + t0_max as i64 + 1) as f64;
        self.delay = self.target_delay(ctl.timing);
        self.cents = self.pitch_noise.value() * ctl.var_cents;
        self.next_analysis = self.next_synth - self.delay as f64;
        self.corr_st = 0.0;
        self.active = true;
    }

    #[inline]
    fn target_delay(&self, timing: f32) -> f32 {
        let u = 0.5 + 0.5 * self.time_noise.value();
        timing * (DELAY_FLOOR + (1.0 - DELAY_FLOOR) * u)
    }

    /// Advance the wander by one sample and smooth toward it.
    #[inline]
    pub fn tick(&mut self, ctl: &VoiceCtl) {
        self.time_noise.advance(1);
        self.pitch_noise.advance(1);
        let d = self.target_delay(ctl.timing);
        let c = self.pitch_noise.value() * ctl.var_cents;
        self.delay += (d - self.delay) * ctl.smooth;
        self.cents += (c - self.cents) * ctl.smooth;
    }

    /// Current lag behind the lead, samples.
    pub fn delay_samples(&self) -> f32 {
        self.delay
    }

    /// The shift the last grain was rendered at, in cents.
    pub fn applied_cents(&self) -> f32 {
        self.applied_st * 100.0
    }

    /// The semitone shift for the grain about to be cut at input pitch `hz`.
    #[inline]
    fn grain_shift_st(&mut self, hz: f32, step: f32, ctl: &VoiceCtl) -> f32 {
        match ctl.tuned {
            None => self.cents * 0.01,
            Some((key, mask, retune)) => {
                if hz >= VOICED_HZ {
                    let target = scale::nearest_correction_st(hz, key, mask)
                        .clamp(-MAX_CORR_ST, MAX_CORR_ST);
                    let k = if retune <= 1.0 { 1.0 } else { 1.0 - (-step / retune).exp() };
                    self.corr_st += (target - self.corr_st) * k;
                }
                self.corr_st
            }
        }
    }

    /// Place every grain whose synthesis mark is before `horizon`.
    pub fn place_grains(&mut self, src: &Source, horizon: f64, ctl: &VoiceCtl) {
        let mut guard = 0u32;
        while self.next_synth < horizon && guard < 64 {
            let s = self.next_synth;
            let q = s - self.delay as f64;
            let hz = src.hz_at(q);
            let t0 = (src.sr / hz).clamp(src.t0_min, src.t0_max);
            let t0d = t0 as f64;
            // Keep the analysis grid within one period of q, either direction:
            // a growing delay walks it back, which is what a late singer is.
            while self.next_analysis + t0d <= q {
                self.next_analysis += t0d;
            }
            while self.next_analysis > q {
                self.next_analysis -= t0d;
            }
            let na = self.next_analysis;
            let center = if q - na > na + t0d - q { na + t0d } else { na };
            let st = self.grain_shift_st(hz, t0, ctl);
            self.applied_st = st;
            let alpha = 2f32.powf(st / 12.0).max(0.05);
            self.place_grain(src, center, s, t0, ctl.beta);
            self.next_synth += t0d / alpha as f64;
            guard += 1;
        }
    }

    /// Overlap-add one 2·T0 Hann grain read around `center` (input time) into
    /// the output at `synth` (output time). `beta` scales the read step, which
    /// moves the formants without touching the mark spacing.
    #[inline]
    fn place_grain(&mut self, src: &Source, center: f64, synth: f64, t0: f32, beta: f32) {
        let ti = t0.round().max(2.0);
        let l = (2.0 * ti) as usize;
        let inv_l = core::f32::consts::TAU / l as f32;
        let oldest = src.write_pos - src.mask as i64;
        for k in 0..l {
            let kf = k as f32;
            let w = 0.5 - 0.5 * (inv_l * kf).cos();
            let pos = center + ((kf - ti) * beta) as f64;
            let ip = pos.floor();
            let frac = (pos - ip) as f32;
            let i0 = (ip as i64).clamp(oldest, src.write_pos);
            let i1 = (i0 + 1).min(src.write_pos);
            let a0 = src.ring[(i0 as usize) & src.mask];
            let a1 = src.ring[(i1 as usize) & src.mask];
            let oi = (synth + (kf - ti) as f64).round() as i64;
            if oi < 0 {
                continue;
            }
            let o = (oi as usize) & self.omask;
            self.out[o] += (a0 + (a1 - a0) * frac) * w;
            self.win[o] += w;
        }
    }

    /// Take the finished sample at output position `r` and clear its slot.
    #[inline]
    pub fn read(&mut self, r: i64) -> f32 {
        if r < 0 {
            return 0.0;
        }
        let o = (r as usize) & self.omask;
        // Normalise by the window sum but never amplify (same rule as the
        // shared PSOLA shifter: thin overlap must not blow grain edges up).
        let v = self.out[o] / self.win[o].max(1.0);
        self.out[o] = 0.0;
        self.win[o] = 0.0;
        if v.is_finite() {
            v
        } else {
            0.0
        }
    }
}
