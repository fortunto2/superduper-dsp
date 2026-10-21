//! Vocal doubler — up to four "re-sung" copies of a voice, each drifting in
//! time and pitch on its own, Revoice-Doubletracker style rather than a static
//! MicroShift (fixed delay + fixed detune).
//!
//! One YIN tracker and one input ring are shared; each [`DoubleVoice`] is a
//! TD-PSOLA synthesis head with its own delay, pitch ratio, level and pan. See
//! `voice.rs` for why the delay has to be applied by grain choice instead of a
//! modulated delay line.
//!
//! Modes:
//! - **Natural** — every double wanders ±`pitch_var_cents` and 0.3–1 × Timing
//!   late, independently.
//! - **Tuned** — the doubles are snapped to the nearest note of Key/Scale at the
//!   Retune speed (an Auto-Tuned double under a natural lead). Pitch Var does
//!   not apply: a tuned double is the flat reference the lead sits against.
//! - **Tuned Lead** — the reverse: the lead goes through the shared PSOLA
//!   shifter snapped to the scale, the doubles stay natural.
//!
//! Latency: the shared `PitchEngine`'s (PSOLA look-behind at a 70 Hz floor,
//! 2744 samples at 48 kHz — the number Pitch and Tune report), fixed across
//! modes so a
//! mode switch never re-triggers host PDC. The dry path is delayed by exactly
//! that, so Mix 0 or Voices 0 is the input, sample for sample, `latency` late.
//!
//! RT-safe: everything is allocated in [`Doubler::new`].

pub mod noise;
pub mod voice;

use crate::pitch::YinPitchTracker;
use crate::pitch_engine::PitchEngine;
use crate::psola::PitchParams;
use crate::scale;
pub use voice::{DoubleVoice, VoiceCtl, DELAY_FLOOR};

pub const MAX_VOICES: usize = 4;
/// Upper end of the Timing param, ms — sizes the input ring.
pub const MAX_TIMING_MS: f32 = 80.0;
/// Lowest tracked fundamental (sets the latency), highest.
pub const MIN_HZ: f32 = 70.0;
pub const MAX_HZ: f32 = 1000.0;
/// YIN window and hop for the shared tracker.
const YIN_WINDOW: usize = 1536;
const YIN_HOP: usize = 256;
/// Pitch history resolution, samples.
const HIST_HOP: usize = 64;
/// Voice fade and lead crossfade time, seconds.
const FADE_S: f32 = 0.02;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubleMode {
    Natural,
    Tuned,
    TunedLead,
}

impl DoubleMode {
    pub fn from_index(v: f32) -> Self {
        match v.round() as i32 {
            1 => Self::Tuned,
            2 => Self::TunedLead,
            _ => Self::Natural,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DoublerParams {
    pub mode: DoubleMode,
    /// 0..=4 doubles.
    pub voices: usize,
    /// Maximum lag of a double behind the lead, ms (it wanders 0.3–1 × this).
    pub timing_ms: f32,
    /// Natural pitch wander depth, ± cents.
    pub pitch_var_cents: f32,
    /// Formant shift of the doubles, semitones.
    pub formant_st: f32,
    /// Stereo spread of the doubles, 0 (all centre) .. 1 (hard L/R).
    pub spread: f32,
    /// Level of the doubles, linear.
    pub level: f32,
    /// Wet amount (doubles), 0..1.
    pub mix: f32,
    /// Lead level, linear.
    pub dry: f32,
    /// Output trim, linear.
    pub output: f32,
    pub seed: u32,
    pub key: u8,
    pub scale_mask: u16,
    pub retune_ms: f32,
    /// Bus-key gain on the doubles, 0..1 (1 = no reaction).
    pub key_gain: f32,
    pub bypassed: bool,
}

impl Default for DoublerParams {
    fn default() -> Self {
        Self {
            mode: DoubleMode::Natural,
            voices: 2,
            timing_ms: 30.0,
            pitch_var_cents: 8.0,
            formant_st: 0.0,
            spread: 0.8,
            level: 10f32.powf(-3.0 / 20.0),
            mix: 1.0,
            dry: 1.0,
            output: 1.0,
            seed: 1,
            key: 0,
            scale_mask: scale::SCALES[0].1,
            retune_ms: 20.0,
            key_gain: 1.0,
            bypassed: false,
        }
    }
}

/// The shared input as a voice sees it.
pub struct Source<'a> {
    pub ring: &'a [f32],
    pub mask: usize,
    pub write_pos: i64,
    pub sr: f32,
    pub t0_min: f32,
    pub t0_max: f32,
    hist: &'a [f32],
    hist_mask: usize,
    /// How far behind its emit time a YIN estimate is centred, samples.
    yin_lag: i64,
}

impl Source<'_> {
    /// Tracked pitch of the input around absolute sample `q`.
    #[inline]
    pub fn hz_at(&self, q: f64) -> f32 {
        let newest = self.write_pos / HIST_HOP as i64;
        let oldest = newest - self.hist_mask as i64;
        let idx = ((q as i64 + self.yin_lag) / HIST_HOP as i64).clamp(oldest.max(0), newest);
        self.hist[(idx as usize) & self.hist_mask]
    }
}

/// One-pole toward `target` that lands on it exactly once close enough, so a
/// gain smoothed to 0 or 1 is bit-exact (Mix 0 must be the dry signal).
#[inline]
fn glide(cur: &mut f32, target: f32, coef: f32) -> f32 {
    *cur += (target - *cur) * coef;
    if (*cur - target).abs() < 1e-6 {
        *cur = target;
    }
    *cur
}

pub struct Doubler {
    sr: f32,
    latency: usize,
    mask: usize,
    ring_l: Box<[f32]>,
    ring_r: Box<[f32]>,
    ring_m: Box<[f32]>,
    hist: Box<[f32]>,
    hist_mask: usize,
    yin_lag: i64,
    tracker: YinPitchTracker,
    t0_min: f32,
    t0_max: f32,
    write_pos: i64,
    voices: [DoubleVoice; MAX_VOICES],
    seed: u32,
    lead: PitchEngine,
    lead_l: Box<[f32]>,
    lead_r: Box<[f32]>,
    lead_running: bool,
    lead_warm: usize,
    lead_blend: f32,
    lead_corr: f32,
    sm_mix: f32,
    sm_dry: f32,
    sm_out: f32,
    sm_key: f32,
    sm_timing: f32,
    coef_param: f32,
    coef_drift: f32,
    fade_step: f32,
}

impl Doubler {
    pub fn new(sr: f32, max_frames: usize) -> Self {
        // The lead engine (Tuned Lead) is the same router Tune uses; its
        // latency is the instance's latency, so a mode switch never moves PDC.
        let lead = PitchEngine::with_floor(sr, max_frames, MIN_HZ);
        let latency = lead.latency_samples() as usize;
        let t0_max = (sr / MIN_HZ).ceil();
        let t0_min = (sr / MAX_HZ).floor().max(4.0);
        let max_delay = (MAX_TIMING_MS * 1e-3 * sr).ceil() as usize;
        let ring = (latency + max_delay + 4 * t0_max as usize + 64).next_power_of_two();
        let window = YIN_WINDOW.max(2 * t0_max as usize + 2);
        let hist_len = ((ring + window) / HIST_HOP + 4).next_power_of_two();
        let out_len = 4 * t0_max as usize + 16;
        let seed = 1;
        Self {
            sr,
            latency,
            mask: ring - 1,
            ring_l: vec![0.0; ring].into_boxed_slice(),
            ring_r: vec![0.0; ring].into_boxed_slice(),
            ring_m: vec![0.0; ring].into_boxed_slice(),
            hist: vec![150.0; hist_len].into_boxed_slice(),
            hist_mask: hist_len - 1,
            yin_lag: (window / 2) as i64,
            tracker: YinPitchTracker::new(sr, MIN_HZ, MAX_HZ, YIN_WINDOW, YIN_HOP, 150.0),
            t0_min,
            t0_max,
            write_pos: 0,
            voices: std::array::from_fn(|i| DoubleVoice::new(out_len, sr, seed, i as u32)),
            seed,
            lead,
            lead_l: vec![0.0; max_frames.max(1)].into_boxed_slice(),
            lead_r: vec![0.0; max_frames.max(1)].into_boxed_slice(),
            lead_running: false,
            lead_warm: 0,
            lead_blend: 0.0,
            lead_corr: 0.0,
            sm_mix: 1.0,
            sm_dry: 1.0,
            sm_out: 1.0,
            sm_key: 1.0,
            sm_timing: 30.0 * 1e-3 * sr,
            coef_param: 1.0 - (-1.0 / (0.005 * sr)).exp(),
            coef_drift: 1.0 - (-1.0 / (0.05 * sr)).exp(),
            fade_step: 1.0 / (FADE_S * sr),
        }
    }

    /// Latency to report to the host, samples. Fixed for the instance.
    pub fn latency_samples(&self) -> u32 {
        self.latency as u32
    }

    /// Snap the smoothed gains to `p` (call at activate, after a state load),
    /// so the first block does not fade in from the defaults.
    pub fn prime(&mut self, p: &DoublerParams) {
        self.sm_mix = p.mix;
        self.sm_dry = p.dry;
        self.sm_out = p.output;
        self.sm_key = p.key_gain;
        self.sm_timing = p.timing_ms.clamp(0.0, MAX_TIMING_MS) * 1e-3 * self.sr;
        self.set_seed(p.seed);
        for (i, v) in self.voices.iter_mut().enumerate() {
            v.active = false;
            v.gain = 0.0;
            if i < p.voices {
                // Start already in: no fade-in on the first block.
                let ctl = VoiceCtl {
                    timing: self.sm_timing,
                    var_cents: p.pitch_var_cents,
                    beta: 1.0,
                    tuned: None,
                    smooth: self.coef_drift,
                };
                v.wake(self.write_pos - self.latency as i64, self.t0_max, &ctl);
                v.gain = 1.0;
            }
        }
    }

    fn set_seed(&mut self, seed: u32) {
        if seed != self.seed {
            self.seed = seed;
            for (i, v) in self.voices.iter_mut().enumerate() {
                v.reseed(seed, i as u32, self.sr);
            }
        }
    }

    /// Input pitch the doubles are cut at, Hz (the tracker's held estimate).
    pub fn tracked_hz(&self) -> f32 {
        self.tracker.current_hz()
    }

    /// Voice `i`'s current lag behind the lead (ms) and pitch offset (cents),
    /// or `None` while it is silent.
    pub fn voice_readout(&self, i: usize) -> Option<(f32, f32)> {
        let v = self.voices.get(i)?;
        if v.dormant() {
            return None;
        }
        Some((v.delay_samples() * 1000.0 / self.sr, v.applied_cents()))
    }

    /// Lead correction currently applied in Tuned Lead mode, semitones.
    pub fn lead_correction_st(&self) -> f32 {
        self.lead_corr * self.lead_blend
    }

    /// Pan law per voice: `n` doubles spread evenly across ±`spread`.
    fn pan_gains(i: usize, n: usize, spread: f32) -> (f32, f32) {
        let pos = if n <= 1 { 0.0 } else { -1.0 + 2.0 * i as f32 / (n - 1) as f32 };
        let theta = (pos * spread.clamp(0.0, 1.0) + 1.0) * core::f32::consts::FRAC_PI_4;
        (theta.cos(), theta.sin())
    }

    /// Run the Tuned Lead shifter over the block into `lead_l/r`, managing its
    /// warm-up so the crossfade only starts once its output is real.
    fn run_lead(&mut self, in_l: &[f32], in_r: &[f32], n: usize, p: &DoublerParams) {
        let want = p.mode == DoubleMode::TunedLead;
        if !want && !self.lead_running {
            return;
        }
        if want && !self.lead_running {
            self.lead.reset();
            self.lead.prime(1.0, 1.0);
            self.lead_running = true;
            self.lead_warm = 0;
            self.lead_corr = 0.0;
        }
        let hz = self.lead.tracked_hz();
        let target = scale::nearest_correction_st(hz, p.key, p.scale_mask).clamp(-12.0, 12.0);
        let rs = p.retune_ms.max(0.0) * 1e-3 * self.sr;
        let k = if rs <= 1.0 { 1.0 } else { 1.0 - (-(n as f32) / rs).exp() };
        self.lead_corr += (target - self.lead_corr) * k;
        let pp = PitchParams {
            pitch_st: self.lead_corr,
            formant_st: 0.0,
            mix: 1.0,
            output_lin: 1.0,
            bypassed: false,
        };
        let (ll, lr) = (&mut self.lead_l[..n], &mut self.lead_r[..n]);
        self.lead.process(&in_l[..n], &in_r[..n], ll, lr, &pp);
        self.lead_warm = self.lead_warm.saturating_add(n);
    }

    /// Block-rate bookkeeping for the voices: seed, wake/sleep, control struct.
    fn voice_ctl(&mut self, p: &DoublerParams) -> VoiceCtl {
        self.set_seed(p.seed);
        let tuned = (p.mode == DoubleMode::Tuned)
            .then(|| (p.key, p.scale_mask, p.retune_ms.max(0.0) * 1e-3 * self.sr));
        let ctl = VoiceCtl {
            timing: self.sm_timing,
            var_cents: p.pitch_var_cents.max(0.0),
            beta: 2f32.powf(p.formant_st.clamp(-12.0, 12.0) / 12.0),
            tuned,
            smooth: self.coef_drift,
        };
        let r = self.write_pos - self.latency as i64;
        for (i, v) in self.voices.iter_mut().enumerate() {
            let want = i < p.voices.min(MAX_VOICES);
            if want && v.dormant() {
                v.wake(r, self.t0_max, &ctl);
            }
            v.active = want;
        }
        ctl
    }

    /// Process one stereo block. For mono pass the same slice twice; an empty
    /// `out_r` is allowed.
    pub fn process(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
        p: &DoublerParams,
    ) {
        let n = in_l.len().min(out_l.len()).min(in_r.len()).min(self.lead_l.len());
        if p.bypassed {
            out_l[..n].copy_from_slice(&in_l[..n]);
            let rn = n.min(out_r.len());
            out_r[..rn].copy_from_slice(&in_r[..rn]);
            return;
        }
        self.run_lead(in_l, in_r, n, p);
        let mut ctl = self.voice_ctl(p);
        let nv = p.voices.min(MAX_VOICES);
        let pans: [(f32, f32); MAX_VOICES] =
            std::array::from_fn(|i| Self::pan_gains(i, nv, p.spread));
        let timing_target = p.timing_ms.clamp(0.0, MAX_TIMING_MS) * 1e-3 * self.sr;
        let want_lead = p.mode == DoubleMode::TunedLead;
        let lead_target = if want_lead && self.lead_warm > self.latency + YIN_WINDOW {
            1.0
        } else {
            0.0
        };
        let level = p.level.max(0.0);

        for i in 0..n {
            self.write_input(in_l[i], in_r[i]);
            glide(&mut self.sm_timing, timing_target, self.coef_drift);
            ctl.timing = self.sm_timing;
            let r = self.write_pos - self.latency as i64;
            let (dl, dr) = self.dry_at(r);
            let (ld, rd) = self.lead_at(i, dl, dr, lead_target, want_lead);
            let (wl, wr) = self.render_voices(r, &ctl, &pans, level);
            let mix = glide(&mut self.sm_mix, p.mix.clamp(0.0, 1.0), self.coef_param);
            let key = glide(&mut self.sm_key, p.key_gain.clamp(0.0, 1.0), self.coef_param);
            let dry = glide(&mut self.sm_dry, p.dry.max(0.0), self.coef_param);
            let og = glide(&mut self.sm_out, p.output.max(0.0), self.coef_param);
            let g = mix * key;
            // Adding an exact zero would still turn a −0.0 input into +0.0;
            // skip it so Mix 0 is the dry signal bit for bit.
            let (mut ol, mut or) = if g == 0.0 || (wl == 0.0 && wr == 0.0) {
                (ld * dry * og, rd * dry * og)
            } else {
                ((ld * dry + wl * g) * og, (rd * dry + wr * g) * og)
            };
            if !ol.is_finite() {
                ol = 0.0;
            }
            if !or.is_finite() {
                or = 0.0;
            }
            out_l[i] = ol;
            if let Some(o) = out_r.get_mut(i) {
                *o = or;
            }
            self.write_pos += 1;
        }
    }

    #[inline]
    fn write_input(&mut self, xl: f32, xr: f32) {
        let w = (self.write_pos as usize) & self.mask;
        self.ring_l[w] = xl;
        self.ring_r[w] = xr;
        let m = 0.5 * (xl + xr);
        self.ring_m[w] = m;
        self.tracker.push(m);
        if (self.write_pos as usize).is_multiple_of(HIST_HOP) {
            let h = (self.write_pos as usize / HIST_HOP) & self.hist_mask;
            self.hist[h] = self.tracker.current_hz();
        }
    }

    #[inline]
    fn dry_at(&self, r: i64) -> (f32, f32) {
        if r < 0 {
            return (0.0, 0.0);
        }
        let a = (r as usize) & self.mask;
        (self.ring_l[a], self.ring_r[a])
    }

    /// The lead: the delayed dry, crossfaded into the tuned lead when asked.
    /// Once fully faded out and no longer wanted, the shifter stops running.
    #[inline]
    fn lead_at(&mut self, i: usize, dl: f32, dr: f32, target: f32, want: bool) -> (f32, f32) {
        if !self.lead_running {
            return (dl, dr);
        }
        let b = if target > self.lead_blend {
            (self.lead_blend + self.fade_step).min(target)
        } else {
            (self.lead_blend - self.fade_step).max(target)
        };
        self.lead_blend = b;
        if b <= 0.0 && !want {
            self.lead_running = false;
            return (dl, dr);
        }
        (dl + (self.lead_l[i] - dl) * b, dr + (self.lead_r[i] - dr) * b)
    }

    /// Sum the doubles at output position `r`, panned and levelled.
    #[inline]
    fn render_voices(
        &mut self,
        r: i64,
        ctl: &VoiceCtl,
        pans: &[(f32, f32); MAX_VOICES],
        level: f32,
    ) -> (f32, f32) {
        let src = Source {
            ring: &self.ring_m,
            mask: self.mask,
            write_pos: self.write_pos,
            sr: self.sr,
            t0_min: self.t0_min,
            t0_max: self.t0_max,
            hist: &self.hist,
            hist_mask: self.hist_mask,
            yin_lag: self.yin_lag,
        };
        let horizon = (r + self.t0_max as i64 + 1) as f64;
        let (mut wl, mut wr) = (0.0f32, 0.0f32);
        for (v, &(pl, pr)) in self.voices.iter_mut().zip(pans.iter()) {
            if v.dormant() {
                continue;
            }
            v.tick(ctl);
            v.place_grains(&src, horizon, ctl);
            let s = v.read(r);
            v.gain = if v.active {
                (v.gain + self.fade_step).min(1.0)
            } else {
                (v.gain - self.fade_step).max(0.0)
            };
            let g = s * v.gain * v.level * level;
            wl += g * pl;
            wr += g * pr;
        }
        (wl, wr)
    }
}
