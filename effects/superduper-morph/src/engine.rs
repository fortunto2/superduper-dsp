//! The Morph engine, host-free: main stereo + sidechain stereo in, morphed
//! stereo out, `LATENCY` samples late. Tests drive this directly; the CLAP
//! layer in `lib.rs` only moves buffers and params.
//!
//! Per hop (N = 2048, hop = 512 → 10.7 ms at 48 kHz):
//! 1. The sidechain's STFT frame is analysed (its own YIN f0 → harmonic
//!    levels by number, envelope, noise) and folded into the live profile.
//! 2. The main mid signal is analysed the same way.
//! 3. The target for this frame is built from the selected source.
//! 4. `timbre::morph_gains` gives one gain per bin; it is smoothed over
//!    time and applied to the L and R spectra (their phases untouched).
//!
//! Morph 0 is an exact identity: below Morph 0.05 the output crossfades to
//! the latency-delayed dry signal, and at 0 it IS that signal, bit for bit.

use superduper_synth_core::dsp_blocks::LatencyDelay;
use superduper_synth_core::pitch::YinPitchTracker;
use superduper_synth_core::spectral::StftProcessor;
use superduper_synth_core::timbre::{
    morph_gains, Analyzer, FrameAnalysis, FrameMode, GainScratch, ModelTarget, MorphSettings,
    TargetFrame, TimbreProfile,
};

pub const N: usize = 2048;
pub const HOP: usize = 512;
/// Reported latency in samples: N = 42.7 ms at 48 kHz.
// AI-NOTE: `StftProcessor::latency()` says N − hop, but its output is measured
// N samples late (a hop's worth of input is buffered before the first frame
// fires). Morph reports and aligns the dry path to the measured N; pinned by
// tests/dsp_smoke.rs `morphing_toward_itself_is_transparent_and_aligned`.
pub const LATENCY: usize = N;
/// Below this Morph the wet path crossfades into the delayed dry.
const DRY_XFADE: f32 = 0.05;
/// Per-hop smoothing of the bin gains (0 = none). ~20 ms.
const GAIN_SMOOTH: f32 = 0.35;
/// Per-hop smoothing of the sidechain's live profile. ~50 ms.
const PROFILE_SMOOTH: f32 = 0.6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Sidechain,
    Captured,
    Model,
}

impl Source {
    pub fn from_param(v: f32) -> Self {
        match v.round() as i32 {
            0 => Source::Sidechain,
            1 => Source::Captured,
            _ => Source::Model,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub morph: f32,
    pub vowels: f32,
    pub breath: f32,
    pub tilt_db_oct: f32,
    pub mix: f32,
    pub output_db: f32,
    pub source: Source,
    /// Harmonic mode (else Envelope).
    pub harmonic: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            morph: 1.0,
            vowels: 0.0,
            breath: 0.3,
            tilt_db_oct: 0.0,
            mix: 1.0,
            output_db: 0.0,
            source: Source::Model,
            harmonic: true,
        }
    }
}

pub struct MorphEngine {
    /// [0]: inputs (mid, L) → L out. [1]: R → R out. [2]: sidechain analysis.
    stft: [StftProcessor; 3],
    yin: YinPitchTracker,
    yin_sc: YinPitchTracker,
    dry: [LatencyDelay; 2],
    an: Analyzer,
    an_sc: Analyzer,
    pub frame: FrameAnalysis,
    pub frame_sc: FrameAnalysis,
    pub sc_live: TimbreProfile,
    pub captured: TimbreProfile,
    pub model: ModelTarget,
    target: TargetFrame,
    pub gains: GainScratch,
    gain_db: Box<[f32]>,
    gain_lin: Box<[f32]>,
    pub mode: FrameMode,
    morph_s: f32,
    mix_s: f32,
    out_s: f32,
    coef: f32,
    fpb: f32,
}

impl MorphEngine {
    pub fn new(sr: f32, model: ModelTarget) -> Self {
        let st = || StftProcessor::new(sr, N, HOP, 1);
        let s0 = StftProcessor::new(sr, N, HOP, 2);
        let half = s0.half();
        let fpb = s0.freq_per_bin();
        Self {
            stft: [s0, st(), st()],
            yin: YinPitchTracker::new(sr, 55.0, 1000.0, 2048, 256, 150.0),
            yin_sc: YinPitchTracker::new(sr, 40.0, 1000.0, 2048, 256, 150.0),
            dry: [LatencyDelay::new(LATENCY), LatencyDelay::new(LATENCY)],
            an: Analyzer::new(half, fpb),
            an_sc: Analyzer::new(half, fpb),
            frame: FrameAnalysis::default(),
            frame_sc: FrameAnalysis::default(),
            sc_live: TimbreProfile::default(),
            captured: TimbreProfile::default(),
            model,
            target: TargetFrame::default(),
            gains: GainScratch::new(),
            gain_db: vec![0.0; half + 1].into_boxed_slice(),
            gain_lin: vec![1.0; half + 1].into_boxed_slice(),
            mode: FrameMode::Pass,
            morph_s: -1.0,
            mix_s: -1.0,
            out_s: -100.0,
            coef: (-1.0 / (0.01 * sr.max(1.0))).exp(),
            fpb,
        }
    }

    /// Freeze the live sidechain timbre. False if nothing audible has arrived.
    pub fn capture(&mut self) -> bool {
        if self.sc_live.env_valid {
            self.captured = self.sc_live;
            true
        } else {
            false
        }
    }

    /// Whether the sidechain's current frame is pitched.
    pub fn sc_voiced(&self) -> bool {
        self.frame_sc.voiced
    }

    #[inline]
    fn smooth(cur: &mut f32, target: f32, coef: f32, snap: f32) {
        if *cur < snap {
            *cur = target; // first sample after activate: no fade-in
        } else {
            *cur = target + (*cur - target) * coef;
            if (*cur - target).abs() < 1e-5 {
                *cur = target;
            }
        }
    }

    /// One stereo sample in (main + sidechain), one out.
    #[inline]
    pub fn process(&mut self, l: f32, r: f32, sc_l: f32, sc_r: f32, s: &Settings) -> (f32, f32) {
        Self::smooth(&mut self.morph_s, s.morph.clamp(0.0, 1.0), self.coef, -0.5);
        Self::smooth(&mut self.mix_s, s.mix.clamp(0.0, 1.0), self.coef, -0.5);
        Self::smooth(&mut self.out_s, s.output_db, self.coef, -99.0);
        let mid = 0.5 * (l + r);
        let scm = 0.5 * (sc_l + sc_r);
        self.yin.push(mid);
        self.yin_sc.push(scm);
        let morph = self.morph_s;

        let MorphEngine {
            stft, yin, yin_sc, an, an_sc, frame, frame_sc, sc_live, captured, model, target,
            gains, gain_db, gain_lin, mode, fpb, ..
        } = self;
        let [st_l, st_r, st_sc] = stft;

        // 1. Sidechain analysis (its synthesis output is discarded).
        st_sc.process_sample(&[scm], |ana, syn| {
            an_sc.analyze(ana[0], yin_sc.current_hz(), frame_sc);
            sc_live.update(frame_sc, PROFILE_SMOOTH);
            syn.iter_mut().for_each(|c| *c = Default::default());
        });

        // 2–4. Main analysis, target, gains, L synthesis.
        let wl = st_l.process_sample(&[mid, l], |ana, syn| {
            an.analyze(ana[0], yin.current_hz(), frame);
            match s.source {
                Source::Sidechain => target.from_profile(sc_live, frame_sc.voiced),
                Source::Captured => target.from_profile(captured, true),
                Source::Model => target.from_model(model, frame.f0),
            }
            let ms = MorphSettings {
                morph,
                vowels: s.vowels.clamp(0.0, 1.0),
                breath: s.breath.clamp(0.0, 1.0),
                tilt_db_oct: s.tilt_db_oct,
                harmonic: s.harmonic,
            };
            *mode = morph_gains(frame, an.power(), *fpb, target, &ms, gains, gain_db);
            for (b, c) in syn.iter_mut().enumerate() {
                let g = 10f32.powf(gain_db[b] * 0.05);
                let gl = &mut gain_lin[b];
                *gl = g + (*gl - g) * GAIN_SMOOTH;
                *c = ana[1][b] * *gl;
            }
        });
        // R reuses this hop's gains (its frame runs right after L's).
        let wr = st_r.process_sample(&[r], |ana, syn| {
            for (b, c) in syn.iter_mut().enumerate() {
                *c = ana[0][b] * gain_lin[b];
            }
        });

        let dl = self.dry[0].process(l);
        let dr = self.dry[1].process(r);
        let c = (morph / DRY_XFADE).min(1.0);
        let (ml, mr) = if c > 0.0 { (dl + c * (wl - dl), dr + c * (wr - dr)) } else { (dl, dr) };
        let m = self.mix_s;
        let (ol, or) = if m < 1.0 { (dl + m * (ml - dl), dr + m * (mr - dr)) } else { (ml, mr) };
        if self.out_s == 0.0 {
            (ol, or)
        } else {
            let g = 10f32.powf(self.out_s * 0.05);
            (ol * g, or * g)
        }
    }
}
